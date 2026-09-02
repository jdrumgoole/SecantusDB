//! The SecantusDB PostgreSQL server -- P1 vertical slice.
//!
//! psql -> `pgwire` -> `secantus-pgplan` (libpg_query) -> MQL ->
//! `secantus-storage` (WiredTiger). No Python anywhere in that path, and no
//! fallback into it: a construct the planner cannot lower becomes a real
//! PostgreSQL SQLSTATE, never a wrong row.
//!
//! Scope is deliberately thin -- CREATE TABLE, INSERT, single-table SELECT --
//! because the point of P1 is to prove the SEAM end to end on real storage,
//! including the shared on-disk catalog format. Breadth is P5's problem.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bson::{Bson, Document};
use bytes::Bytes;
use futures::{stream, Sink, SinkExt, StreamExt};
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::copy::CopyHandler;
use pgwire::api::portal::{Format, Portal};
use pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{
    CopyEncoder, CopyResponse, CopyTextOptions, DescribePortalResponse, DescribeStatementResponse,
};
use pgwire::api::results::{DataRowEncoder, FieldFormat, FieldInfo, QueryResponse, Response, Tag};
use pgwire::api::stmt::{QueryParser, StoredStatement};
use pgwire::api::{ClientInfo, ClientPortalStore, PgWireServerHandlers, Type};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::copy::{CopyData, CopyDone, CopyFail};
use pgwire::messages::response::CommandComplete;
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use secantus_pgcatalog::{TableDef, CATALOG_COLLECTION};
use secantus_pgplan::{
    companion_field, render_array_element_text, render_timestamp, AggFunc, AggItem, ConstCol,
    Error as PlanError, Nulls, OrderKey, OutputCol, Statement, TransactionControl,
};
use secantus_storage::{Storage, UserTransactionHandle};

/// One database's worth of SQL over a shared `Storage`.
pub struct PgHandler {
    storage: Arc<Storage>,
    db: String,
    /// The open explicit transaction, if any.
    ///
    /// One handler per connection, so this is per-session state exactly as
    /// PostgreSQL's is. Held as a real `UserTransactionHandle` rather than a
    /// flag: a `ROLLBACK` that did not actually roll back would be a silent
    /// wrong answer, which is worse than refusing `BEGIN` outright.
    txn: Mutex<Option<UserTransactionHandle>>,
    /// Session settings (GUCs), per connection as PostgreSQL's are.
    settings: Mutex<HashMap<String, String>>,
    /// The in-progress `COPY ... FROM STDIN`, if any: target plus the bytes
    /// received so far. Per connection, like PostgreSQL's.
    copy_in: Mutex<Option<CopyInState>>,
}

struct CopyInState {
    table: String,
    /// Stored field per target column, in the order the data supplies them.
    fields: Vec<String>,
    /// The declared PostgreSQL type of each target column, for parsing.
    types: Vec<String>,
    buffer: Vec<u8>,
}

impl PgHandler {
    pub fn new(storage: Arc<Storage>, db: &str) -> Self {
        Self {
            storage,
            db: db.to_string(),
            txn: Mutex::new(None),
            settings: Mutex::new(default_settings()),
            copy_in: Mutex::new(None),
        }
    }

    /// Read one table's catalog entry. Reads it back from storage every time
    /// rather than caching: the store is shared with the other two servers, so
    /// a cache here would go stale behind our back.
    fn lookup(&self, name: &str) -> Option<TableDef> {
        let filter = bson::doc! { "_id": name };
        let rows = self
            .storage
            .find_matching(&self.db, CATALOG_COLLECTION, &filter)
            .ok()?;
        let raw = rows.first()?;
        let d: Document = bson::from_slice(raw).ok()?;
        TableDef::from_document(&d)
    }

    fn err(e: &PlanError) -> PgWireError {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            e.sqlstate().into(),
            e.to_string(),
        )))
    }

    /// A storage write error, rendered as PostgreSQL renders it.
    ///
    /// The storage layer speaks MongoDB: a duplicate `_id` comes back as
    /// `E11000 duplicate key error collection: postgres.t index: _id_ ...`.
    /// Letting that reach a psql client would leak the Mongo persona straight
    /// through the PostgreSQL one. Real PostgreSQL 14 answers (probed
    /// 2026-08-31):
    ///
    /// ```text
    /// ERROR:  duplicate key value violates unique constraint "dupt_pkey"
    /// DETAIL:  Key (id)=(1) already exists.
    /// ```
    fn write_error(table: &str, def: &TableDef, err: &Document) -> PgWireError {
        let msg = err.get_str("errmsg").unwrap_or_default();
        if err.get_i32("code").unwrap_or(0) == 11000 || msg.starts_with("E11000") {
            let pk = def.columns.iter().find(|c| c.pk);
            let detail = pk.map(|c| {
                // `keyValue` carries the offending value under the STORED field.
                let v = err
                    .get_document("keyValue")
                    .ok()
                    .and_then(|kv| kv.get(c.field()).cloned());
                match v {
                    Some(Bson::String(s)) => format!("Key ({})=({}) already exists.", c.name, s),
                    Some(b) => format!(
                        "Key ({})=({}) already exists.",
                        c.name,
                        b.to_string().trim_matches('"')
                    ),
                    None => format!("Key ({}) already exists.", c.name),
                }
            });
            let mut info = ErrorInfo::new(
                "ERROR".into(),
                "23505".into(),
                format!("duplicate key value violates unique constraint \"{table}_pkey\""),
            );
            info.detail = detail;
            // pgwire 0.39 added the protocol's schema/table/column/constraint
            // fields (they were absent in 0.31, which is why this used to be a
            // recorded limitation). Real PostgreSQL sends them on a 23505, and
            // pgjdbc surfaces them as
            // `PSQLException.getServerErrorMessage().getConstraint()`.
            info.table = Some(table.to_string());
            info.schema = Some("public".to_string());
            info.constraint = Some(format!("{table}_pkey"));
            // `column` stays UNSET: PostgreSQL identifies the offending column
            // through the constraint on a 23505, not through this field
            // (probed 14 -- it sends None). Populating it looked more helpful
            // and was simply wrong.
            return PgWireError::UserError(Box::new(info));
        }
        Self::storage_err("could not insert", msg)
    }

    fn storage_err(context: &str, e: impl std::fmt::Display) -> PgWireError {
        // A storage failure is never dressed up as a SQL-level error: this is a
        // database, and an internal error must read as one.
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "XX000".into(),
            format!("{context}: {e}"),
        )))
    }
}

/// PostgreSQL's canonical spelling for a setting name.
///
/// `SHOW datestyle` answers a column called `DateStyle` -- lookups are
/// case-insensitive but the reported name is not, and clients match on it.
fn canonical_setting(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "datestyle" => "DateStyle".to_string(),
        "timezone" => "TimeZone".to_string(),
        "intervalstyle" => "IntervalStyle".to_string(),
        other => other.to_string(),
    }
}

/// The settings a fresh connection starts with, matching what a client expects
/// to read back before it has set anything.
fn default_settings() -> HashMap<String, String> {
    [
        ("client_encoding", "UTF8"),
        ("DateStyle", "ISO, MDY"),
        ("TimeZone", "UTC"),
        ("IntervalStyle", "postgres"),
        ("standard_conforming_strings", "on"),
        ("integer_datetimes", "on"),
        ("transaction_read_only", "off"),
        ("search_path", "\"$user\", public"),
        ("application_name", ""),
        ("server_encoding", "UTF8"),
        ("server_version", "15.0"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// The PostgreSQL type a column's declared type maps onto over the wire.
fn wire_type(pg_type: &str) -> Type {
    match pg_type {
        "int2" => Type::INT2,
        "int4" | "integer" | "int" => Type::INT4,
        "int8" | "bigint" => Type::INT8,
        "float4" => Type::FLOAT4,
        "float8" => Type::FLOAT8,
        "bool" | "boolean" => Type::BOOL,
        // `text` is oid 25, NOT varchar (1043). PostgreSQL distinguishes them
        // and clients read the oid: psycopg decodes both to `str` so a value
        // comparison never notices, but pgjdbc and pgx do.
        "text" => Type::TEXT,
        "varchar" | "character varying" => Type::VARCHAR,
        "bpchar" | "char" | "character" => Type::BPCHAR,
        "name" => Type::NAME,
        // Stored as canonical TEXT but reported with their real oids: a client
        // reading 1082/1083 parses the value into a date/time object, whereas
        // varchar hands it back as a string. Same shape as the text-vs-varchar
        // bug -- psycopg would not notice, pgjdbc and pgx would.
        "date" => Type::DATE,
        "time" => Type::TIME,
        "timestamp" => Type::TIMESTAMP,
        // Their own oids: a client reading 1184 builds an aware datetime,
        // where 1114 builds a naive one from the same characters.
        "timestamptz" => Type::TIMESTAMPTZ,
        "timetz" => Type::TIMETZ,
        "interval" => Type::INTERVAL,
        "json" => Type::JSON,
        "jsonb" => Type::JSONB,
        "numeric" | "decimal" => Type::NUMERIC,
        // `pg_typeof` answers a `regtype` (2206), not text: a client reading
        // 25 would print the same characters but compare unequal to a regtype.
        "regtype" => Type::REGTYPE,
        // Array oids are their own types (int4[] is 1007, not 23).
        "int4[]" | "int[]" | "integer[]" => Type::INT4_ARRAY,
        "int8[]" | "bigint[]" => Type::INT8_ARRAY,
        "int2[]" | "smallint[]" => Type::INT2_ARRAY,
        "float8[]" | "double[]" => Type::FLOAT8_ARRAY,
        "float4[]" | "real[]" => Type::FLOAT4_ARRAY,
        "bool[]" | "boolean[]" => Type::BOOL_ARRAY,
        "numeric[]" | "decimal[]" => Type::NUMERIC_ARRAY,
        "text[]" => Type::TEXT_ARRAY,
        "varchar[]" => Type::VARCHAR_ARRAY,
        // Everything else renders as text for now; P4 owns the real type map.
        _ => Type::VARCHAR,
    }
}

#[async_trait]
impl NoopStartupHandler for PgHandler {
    async fn post_startup<C>(&self, _c: &mut C, _m: PgWireFrontendMessage) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        Ok(())
    }
}

#[async_trait]
impl SimpleQueryHandler for PgHandler {
    async fn do_query<C>(&self, _c: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        // The simple protocol takes any number of commands separated by
        // semicolons and answers with one result each. The extended protocol
        // does not, and still refuses -- see `Error::MultipleCommands`.
        let stmts = secantus_pgplan::split_statements(query).map_err(|e| Self::err(&e))?;
        if stmts.len() <= 1 {
            // The one-statement path is left exactly as it was, so the common
            // case cannot be changed by the batching logic below.
            return self.run(query, &[], 0).await;
        }
        self.run_batch(&stmts).await
    }
}

impl PgHandler {
    /// Run a multi-command simple query as PostgreSQL does: every command in
    /// order, one result each, the whole batch in an IMPLICIT TRANSACTION.
    ///
    /// The transaction is the part that is easy to miss and impossible to fake
    /// afterwards. Measured against PostgreSQL 14:
    ///
    /// * `insert into t values (1); select * from nosuchtable` leaves **no**
    ///   row in `t` -- the failure rolls the earlier write back.
    /// * `begin; insert into t values (2); commit; select * from nosuchtable`
    ///   leaves the row -- an explicit COMMIT inside the batch ends the
    ///   transaction, and what it committed survives the later failure.
    ///
    /// Both fall out of reusing the session's own transaction slot rather than
    /// tracking a second one: `BEGIN` inside an open transaction is already a
    /// no-op here (as it is a warning in PostgreSQL), and `COMMIT` already
    /// takes the handle. After a mid-batch COMMIT a fresh implicit transaction
    /// is opened for the commands that follow, which is what PostgreSQL does.
    async fn run_batch(&self, stmts: &[String]) -> PgWireResult<Vec<Response>> {
        let implicit = self.txn.lock().unwrap_or_else(|e| e.into_inner()).is_none();
        if implicit {
            self.begin_implicit()?;
        }

        let mut out = Vec::with_capacity(stmts.len());
        for (i, sql) in stmts.iter().enumerate() {
            match self.run(sql, &[], 0).await {
                Ok(responses) => out.extend(responses),
                Err(e) => {
                    if implicit {
                        // Roll back whatever this batch opened. A failure to
                        // roll back must not mask the error that caused it.
                        let _ = self.rollback_implicit();
                    }
                    return Err(e);
                }
            }
            // An explicit COMMIT or ROLLBACK inside the batch closed the
            // transaction; PostgreSQL starts another for what follows.
            if implicit
                && i + 1 < stmts.len()
                && self.txn.lock().unwrap_or_else(|e| e.into_inner()).is_none()
            {
                self.begin_implicit()?;
            }
        }

        if implicit {
            self.commit_implicit()?;
        }
        Ok(out)
    }

    /// The session's `TimeZone` GUC, resolved.
    fn session_timezone(&self) -> secantus_pgplan::TimeZoneSetting {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        settings
            .get("TimeZone")
            .map(|v| secantus_pgplan::TimeZoneSetting::parse(v))
            .unwrap_or_default()
    }

    fn begin_implicit(&self) -> PgWireResult<()> {
        let handle = self
            .storage
            .begin_user_transaction()
            .map_err(|e| Self::storage_err("could not begin a transaction", e))?;
        *self.txn.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(())
    }

    fn commit_implicit(&self) -> PgWireResult<()> {
        if let Some(mut handle) = self.txn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.storage
                .commit_user_transaction(&mut handle)
                .map_err(|e| Self::storage_err("could not commit a transaction", e))?;
        }
        Ok(())
    }

    fn rollback_implicit(&self) -> PgWireResult<()> {
        if let Some(mut handle) = self.txn.lock().unwrap_or_else(|e| e.into_inner()).take() {
            self.storage
                .rollback_user_transaction(&mut handle)
                .map_err(|e| Self::storage_err("could not roll back a transaction", e))?;
        }
        Ok(())
    }

    /// Execute one statement. Shared by BOTH protocols so they cannot drift:
    /// the simple path passes no parameters, the extended path passes the
    /// portal's bound values and `Execute`'s row limit.
    async fn run(
        &self,
        query: &str,
        params: &[Bson],
        max_rows: usize,
    ) -> PgWireResult<Vec<Response>> {
        let sql = query.trim().trim_end_matches(';').trim();
        if sql.is_empty() {
            return Ok(vec![Response::EmptyQuery]);
        }

        // The session's TimeZone is part of what a statement MEANS: the same
        // `'2026-01-01 12:00'::timestamptz` names a different instant under a
        // different zone, so it is passed in rather than defaulted.
        let stmt = secantus_pgplan::plan_with_session(
            sql,
            &|n| self.lookup(n),
            params,
            &self.session_timezone(),
        )
        .map_err(|e| Self::err(&e))?;

        // Transaction control is session state, not a storage operation.
        if let Statement::Transaction(control) = stmt {
            return self.transaction_control(control);
        }

        // Everything else runs INSIDE the open transaction when there is one,
        // so a later ROLLBACK really discards it.
        let mut guard = self.txn.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(handle) => self
                .storage
                .with_user_transaction(handle, || self.execute(stmt, max_rows))
                .map_err(|e| Self::storage_err("transaction failed", e))?,
            None => self.execute(stmt, max_rows),
        }
    }

    /// Resolve a FROM-less SELECT column that needs connection state.
    fn resolve_const_col(&self, col: &ConstCol) -> PgWireResult<Bson> {
        match col {
            ConstCol::Value(v) => Ok(v.clone()),
            ConstCol::CurrentSetting { name, missing_ok } => {
                let key = canonical_setting(name);
                let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                match settings.get(&key) {
                    Some(v) => Ok(Bson::String(v.clone())),
                    // `current_setting(x)` errors on an unknown name;
                    // `current_setting(x, true)` answers NULL (probed PG 14).
                    None if *missing_ok => Ok(Bson::Null),
                    None => Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42704".into(),
                        format!("unrecognized configuration parameter \"{name}\""),
                    )))),
                }
            }
            ConstCol::SetConfig { name, value, .. } => {
                // `is_local` is accepted and ignored: this server has no
                // statement-scoped settings, and the difference is only
                // observable across a rollback.
                let text = match value {
                    Bson::String(s) => s.clone(),
                    Bson::Null => String::new(),
                    other => format!("{other}"),
                };
                let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                settings.insert(canonical_setting(name), text.clone());
                Ok(Bson::String(text))
            }
        }
    }

    /// BEGIN / COMMIT / ROLLBACK.
    fn transaction_control(&self, control: TransactionControl) -> PgWireResult<Vec<Response>> {
        let mut guard = self.txn.lock().unwrap_or_else(|e| e.into_inner());
        let tag = match control {
            TransactionControl::Begin => {
                if guard.is_none() {
                    let handle = self
                        .storage
                        .begin_user_transaction()
                        .map_err(|e| Self::storage_err("could not begin a transaction", e))?;
                    *guard = Some(handle);
                }
                // A BEGIN inside a transaction is a WARNING in PostgreSQL, not
                // an error, and the existing transaction continues.
                "BEGIN"
            }
            TransactionControl::Commit => {
                if let Some(mut handle) = guard.take() {
                    self.storage
                        .commit_user_transaction(&mut handle)
                        .map_err(|e| Self::storage_err("could not commit", e))?;
                }
                "COMMIT"
            }
            TransactionControl::Rollback => {
                if let Some(mut handle) = guard.take() {
                    self.storage
                        .rollback_user_transaction(&mut handle)
                        .map_err(|e| Self::storage_err("could not roll back", e))?;
                }
                "ROLLBACK"
            }
        };
        Ok(vec![Response::Execution(Tag::new(tag))])
    }

    /// Execute one planned statement against storage.
    fn execute(&self, stmt: Statement, max_rows: usize) -> PgWireResult<Vec<Response>> {
        match stmt {
            Statement::Transaction(_) => unreachable!("handled before execute"),
            Statement::CreateTable(def) => {
                if self.lookup(&def.name).is_some() {
                    return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42P07".into(), // duplicate_table
                        format!("relation \"{}\" already exists", def.name),
                    ))));
                }
                self.storage
                    .create_collection(&self.db, &def.name)
                    .map_err(|e| Self::storage_err("could not create the table", e))?;
                let bytes = bson::to_vec(&def.to_document())
                    .map_err(|e| Self::storage_err("could not encode the catalog entry", e))?;
                self.storage
                    .insert(&self.db, CATALOG_COLLECTION, vec![bytes], true)
                    .map_err(|e| Self::storage_err("could not record the table", e))?;
                Ok(vec![Response::Execution(Tag::new("CREATE TABLE"))])
            }

            Statement::Insert(ins) => {
                let def = self
                    .lookup(&ins.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(ins.table.clone())))?;
                let n = ins.rows.len();
                let docs = ins
                    .rows
                    .iter()
                    .map(bson::to_vec)
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| Self::storage_err("could not encode a row", e))?;
                let (written, errors) = self
                    .storage
                    .insert(&self.db, &ins.table, docs, true)
                    .map_err(|e| Self::storage_err("could not insert", e))?;
                if let Some(first) = errors.first() {
                    return Err(Self::write_error(&ins.table, &def, first));
                }
                debug_assert_eq!(written, n);
                Ok(vec![Response::Execution(
                    Tag::new("INSERT").with_oid(0).with_rows(written),
                )])
            }

            Statement::Select(sel) => {
                let raw = self
                    .storage
                    .find_matching(&self.db, &sel.table, &sel.filter)
                    .map_err(|e| Self::storage_err("could not read", e))?;
                let def = self
                    .lookup(&sel.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(sel.table.clone())))?;

                // Decode once: ORDER BY, OFFSET and LIMIT all need the values,
                // and re-decoding per comparison would be quadratic.
                let mut docs: Vec<Document> = raw
                    .iter()
                    .map(|b| bson::from_slice(b))
                    .collect::<Result<_, _>>()
                    .map_err(|e| Self::storage_err("could not decode a row", e))?;

                if !sel.order.is_empty() {
                    sort_rows(&mut docs, &sel.order);
                }
                // OFFSET is applied before LIMIT, as PostgreSQL does.
                if sel.offset > 0 {
                    let skip = usize::try_from(sel.offset).unwrap_or(usize::MAX);
                    docs = docs.into_iter().skip(skip).collect();
                }
                if let Some(limit) = sel.limit {
                    // A negative LIMIT is a PostgreSQL error, but the parser
                    // hands it through; clamp rather than panic on the cast.
                    let take = usize::try_from(limit.max(0)).unwrap_or(usize::MAX);
                    docs.truncate(take);
                }
                // `Execute` may cap rows independently of any SQL LIMIT; 0 means
                // "no cap" in the protocol, not "no rows".
                if max_rows > 0 {
                    docs.truncate(max_rows);
                }

                let schema = Arc::new(
                    sel.columns
                        .iter()
                        .map(|(out, _)| {
                            let ty = def
                                .column(out)
                                .map(|c| wire_type(&c.pg_type))
                                .unwrap_or(Type::VARCHAR);
                            FieldInfo::new(out.clone(), None, None, ty, FieldFormat::Text)
                        })
                        .collect::<Vec<_>>(),
                );

                let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
                let schema_ref = schema.clone();
                let rows = stream::iter(docs).map(move |d| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    for f in &fields {
                        // A timestamp is reassembled from its stored date plus
                        // the hidden companion before it goes on the wire.
                        match timestamp_text(&d, f) {
                            Some(text) => enc.encode_field(&Some(text.as_str()))?,
                            None => encode_value(&mut enc, d.get(f))?,
                        }
                    }
                    Ok(enc.take_row())
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }

            Statement::DropTable(drop) => {
                for table in &drop.tables {
                    if self.lookup(table).is_none() {
                        if drop.if_exists {
                            continue;
                        }
                        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                            "ERROR".into(),
                            "42P01".into(), // undefined_table
                            format!("table \"{table}\" does not exist"),
                        ))));
                    }
                    // Both halves, and the CATALOG entry last: if the drop
                    // fails midway, a table whose catalog row survived is
                    // recoverable, whereas a catalog row pointing at a
                    // collection that no longer exists is not.
                    self.storage
                        .drop_collection(&self.db, table)
                        .map_err(|e| Self::storage_err("could not drop the table", e))?;
                    self.storage
                        .delete_matching(
                            &self.db,
                            CATALOG_COLLECTION,
                            &bson::doc! { "_id": table },
                            0,
                            &Document::new(),
                            None,
                        )
                        .map_err(|e| Self::storage_err("could not drop the catalog entry", e))?;
                }
                Ok(vec![Response::Execution(Tag::new("DROP TABLE"))])
            }

            Statement::CopyFrom(cf) => {
                let def = self
                    .lookup(&cf.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(cf.table.clone())))?;
                let cols: Vec<&secantus_pgcatalog::Column> = if cf.columns.is_empty() {
                    def.columns.iter().collect()
                } else {
                    cf.columns
                        .iter()
                        .map(|n| def.column(n).expect("planner checked"))
                        .collect()
                };
                let n = cols.len();
                *self.copy_in.lock().unwrap_or_else(|e| e.into_inner()) = Some(CopyInState {
                    table: cf.table.clone(),
                    fields: cols.iter().map(|c| c.field()).collect(),
                    types: cols.iter().map(|c| c.pg_type.clone()).collect(),
                    buffer: Vec::new(),
                });
                // format 0 = text, and every column is text-formatted.
                // The stream is the COPY OUT direction; an IN response has none.
                Ok(vec![Response::CopyIn(CopyResponse::new(
                    0,
                    n,
                    futures::stream::empty(),
                ))])
            }

            Statement::CopyTo(ct) => {
                let def = self
                    .lookup(&ct.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(ct.table.clone())))?;
                let cols: Vec<&secantus_pgcatalog::Column> = if ct.columns.is_empty() {
                    def.columns.iter().collect()
                } else {
                    ct.columns
                        .iter()
                        .map(|n| def.column(n).expect("planner checked"))
                        .collect()
                };
                let schema = Arc::new(
                    cols.iter()
                        .map(|c| {
                            FieldInfo::new(
                                c.name.clone(),
                                None,
                                None,
                                wire_type(&c.pg_type),
                                FieldFormat::Text,
                            )
                        })
                        .collect::<Vec<_>>(),
                );
                let fields: Vec<String> = cols.iter().map(|c| c.field()).collect();

                let raw = self
                    .storage
                    .find_matching(&self.db, &ct.table, &Document::new())
                    .map_err(|e| Self::storage_err("could not read", e))?;
                let docs: Vec<Document> = raw
                    .iter()
                    .map(|b| bson::from_slice(b))
                    .collect::<Result<_, _>>()
                    .map_err(|e| Self::storage_err("could not decode a row", e))?;

                // The text encoder writes PostgreSQL's own escaping -- `\N`
                // for NULL, and escaped tabs/newlines -- so this round-trips
                // through the COPY FROM path above.
                let mut encoder = CopyEncoder::new_text(schema.clone(), CopyTextOptions::default());
                let n = schema.len();
                let data = stream::iter(docs).map(move |d| {
                    for f in &fields {
                        match d.get(f) {
                            Some(Bson::Int32(v)) => encoder.encode_field(&Some(*v))?,
                            Some(Bson::Int64(v)) => encoder.encode_field(&Some(*v))?,
                            Some(Bson::Double(v)) => encoder.encode_field(&Some(*v))?,
                            Some(Bson::Boolean(v)) => encoder.encode_field(&Some(*v))?,
                            Some(Bson::String(v)) => encoder.encode_field(&Some(v.as_str()))?,
                            _ => encoder.encode_field(&None::<i32>)?,
                        }
                    }
                    Ok(encoder.take_copy())
                });
                // format 0 = text.
                Ok(vec![Response::CopyOut(CopyResponse::new(0, n, data))])
            }

            Statement::Show(name) => {
                let key = canonical_setting(&name);
                let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                let value = settings.get(&key).cloned().ok_or_else(|| {
                    PgWireError::UserError(Box::new(ErrorInfo::new(
                        "ERROR".into(),
                        "42704".into(), // undefined_object
                        format!("unrecognized configuration parameter \"{name}\""),
                    )))
                })?;
                let schema = Arc::new(vec![FieldInfo::new(
                    key,
                    None,
                    None,
                    Type::TEXT,
                    FieldFormat::Text,
                )]);
                let schema_ref = schema.clone();
                let rows = stream::iter(std::iter::once(value)).map(move |v| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    enc.encode_field(&Some(v.as_str()))?;
                    Ok(enc.take_row())
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }

            // The wire layer owns the prepared-statement store, so there is
            // nothing here to free: psycopg issues this to reset its own cache
            // and then re-prepares under fresh names. The TAG is the part that
            // matters, and PostgreSQL's is `DEALLOCATE ALL`, not `DEALLOCATE`.
            Statement::DeallocateAll => Ok(vec![Response::Execution(Tag::new("DEALLOCATE ALL"))]),

            Statement::Set { name, value } => {
                let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                settings.insert(canonical_setting(&name), value);
                Ok(vec![Response::Execution(Tag::new("SET"))])
            }

            Statement::Reset(name) => {
                let mut settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                if name.is_empty() {
                    *settings = default_settings();
                } else {
                    let key = canonical_setting(&name);
                    // RESET restores the DEFAULT, which is not the same as
                    // removing the setting: a client reading it back afterwards
                    // must see the default, not an error.
                    match default_settings().get(&key) {
                        Some(d) => {
                            settings.insert(key, d.clone());
                        }
                        None => {
                            settings.remove(&key);
                        }
                    }
                }
                Ok(vec![Response::Execution(Tag::new("RESET"))])
            }

            Statement::SelectConstant(sc) => {
                // One row, no storage touched.
                let schema = Arc::new(
                    sc.columns
                        .iter()
                        .map(|(name, _, ty)| {
                            FieldInfo::new(
                                name.clone(),
                                None,
                                None,
                                wire_type(ty),
                                FieldFormat::Text,
                            )
                        })
                        .collect::<Vec<_>>(),
                );
                let values: Vec<Bson> = sc
                    .columns
                    .iter()
                    .map(|(_, c, _)| self.resolve_const_col(c))
                    .collect::<PgWireResult<Vec<_>>>()?;
                let schema_ref = schema.clone();
                let rows = stream::iter(std::iter::once(values)).map(move |vals| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    for v in &vals {
                        encode_value(&mut enc, Some(v))?;
                    }
                    Ok(enc.take_row())
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }

            Statement::Aggregate(agg) => {
                let raw = self
                    .storage
                    .find_matching(&self.db, &agg.table, &agg.filter)
                    .map_err(|e| Self::storage_err("could not read", e))?;
                let docs: Vec<Document> = raw
                    .iter()
                    .map(|b| bson::from_slice(b))
                    .collect::<Result<_, _>>()
                    .map_err(|e| Self::storage_err("could not decode a row", e))?;

                // Group, preserving first-seen order so output is deterministic
                // even with no ORDER BY.
                let mut keys: Vec<Vec<Option<Bson>>> = Vec::new();
                let mut buckets: Vec<Vec<Document>> = Vec::new();
                if agg.group_by.is_empty() {
                    keys.push(Vec::new());
                    buckets.push(docs);
                } else {
                    for d in docs {
                        // NULL forms its OWN group in PostgreSQL, so a missing
                        // or null key is a real key rather than a skip.
                        let key: Vec<Option<Bson>> = agg
                            .group_by
                            .iter()
                            .map(|(_, f)| match d.get(f) {
                                None | Some(Bson::Null) => None,
                                Some(v) => Some(v.clone()),
                            })
                            .collect();
                        match keys.iter().position(|k| *k == key) {
                            Some(i) => buckets[i].push(d),
                            None => {
                                keys.push(key);
                                buckets.push(vec![d]);
                            }
                        }
                    }
                }

                // (group key, computed aggregates) per group. Kept POSITIONAL:
                // `SELECT count(*), count(n)` yields two columns both named
                // `count`, so a name-keyed row silently drops one.
                let mut groups: Vec<(Vec<Option<Bson>>, Vec<Bson>)> = keys
                    .iter()
                    .zip(buckets.iter())
                    .map(|(k, bucket)| {
                        let vals = agg
                            .items
                            .iter()
                            .map(|item| compute_aggregate(item, bucket))
                            .collect();
                        (k.clone(), vals)
                    })
                    .collect();

                // Sort on the GROUP KEY, by index -- so `GROUP BY s ORDER BY s`
                // works even when `s` is not projected.
                if !agg.order.is_empty() {
                    groups.sort_by(|a, b| {
                        for key in &agg.order {
                            let (l, r) = (&a.0[key.group_index], &b.0[key.group_index]);
                            let ord = match (l, r) {
                                (None, None) => Ordering::Equal,
                                (None, Some(_)) => match key.nulls {
                                    Nulls::First => Ordering::Less,
                                    Nulls::Last => Ordering::Greater,
                                },
                                (Some(_), None) => match key.nulls {
                                    Nulls::First => Ordering::Greater,
                                    Nulls::Last => Ordering::Less,
                                },
                                (Some(x), Some(y)) => {
                                    let c = compare_values(x, y);
                                    if key.ascending {
                                        c
                                    } else {
                                        c.reverse()
                                    }
                                }
                            };
                            if ord != Ordering::Equal {
                                return ord;
                            }
                        }
                        Ordering::Equal
                    });
                }
                if agg.offset > 0 {
                    let skip = usize::try_from(agg.offset).unwrap_or(usize::MAX);
                    groups = groups.into_iter().skip(skip).collect();
                }
                if let Some(limit) = agg.limit {
                    groups.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
                }
                if max_rows > 0 {
                    groups.truncate(max_rows);
                }

                let def = self
                    .lookup(&agg.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(agg.table.clone())))?;
                let schema = Arc::new(
                    agg.select
                        .iter()
                        .map(|(name, col)| {
                            let ty = match col {
                                OutputCol::Group(i) => def
                                    .column(&agg.group_by[*i].0)
                                    .map(|c| wire_type(&c.pg_type))
                                    .unwrap_or(Type::VARCHAR),
                                OutputCol::Agg(i) => aggregate_wire_type(&agg.items[*i]),
                            };
                            FieldInfo::new(name.clone(), None, None, ty, FieldFormat::Text)
                        })
                        .collect::<Vec<_>>(),
                );

                let select = agg.select.clone();
                let schema_ref = schema.clone();
                let rows = stream::iter(groups).map(move |(key, vals)| {
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    for (_, col) in &select {
                        let v = match col {
                            OutputCol::Group(i) => key[*i].clone().unwrap_or(Bson::Null),
                            OutputCol::Agg(i) => vals[*i].clone(),
                        };
                        encode_value(&mut enc, Some(&v))?;
                    }
                    Ok(enc.take_row())
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }

            Statement::Update(upd) => {
                let outcome = self
                    .storage
                    .update_matching(
                        &self.db,
                        &upd.table,
                        &upd.filter,
                        &{
                            let mut ops = bson::doc! { "$set": upd.set };
                            if !upd.unset.is_empty() {
                                let mut u = Document::new();
                                for f in &upd.unset {
                                    u.insert(f.clone(), "");
                                }
                                ops.insert("$unset", u);
                            }
                            ops
                        },
                        true,  // multi: SQL UPDATE has no single-row default
                        false, // upsert: never; PostgreSQL UPDATE does not insert
                        &[],
                        &Document::new(),
                        None,
                        None,
                        false,
                    )
                    .map_err(|e| Self::storage_err("could not update", e))?;
                // PostgreSQL's UPDATE tag counts rows MATCHED, not rows whose
                // value actually changed: `UPDATE t SET n = n` reports every
                // row. `modified` would under-report a no-op assignment.
                Ok(vec![Response::Execution(
                    Tag::new("UPDATE").with_rows(outcome.matched),
                )])
            }

            Statement::Delete(del) => {
                let deleted = self
                    .storage
                    .delete_matching(&self.db, &del.table, &del.filter, 0, &Document::new(), None)
                    .map_err(|e| Self::storage_err("could not delete", e))?;
                Ok(vec![Response::Execution(
                    Tag::new("DELETE").with_rows(deleted),
                )])
            }
        }
    }
}

/// A stored timestamp, with its hidden companion added back.
///
/// The remainder is validated rather than trusted: a value outside 0-999, or
/// not an integer at all, is ignored. A hand-edited or foreign document must
/// not be able to produce a time that was never written -- the same
/// defensiveness `subms.py::merge` applies on the Python side.
fn timestamp_text(doc: &Document, field: &str) -> Option<String> {
    let ms = match doc.get(field) {
        Some(Bson::DateTime(d)) => d.timestamp_millis(),
        _ => return None,
    };
    let rem = match doc.get(companion_field(field)) {
        Some(Bson::Int32(v)) if (1..1000).contains(v) => i64::from(*v),
        Some(Bson::Int64(v)) if (1..1000).contains(v) => *v,
        _ => 0,
    };
    Some(render_timestamp(ms * 1000 + rem))
}

/// Encode one stored value as a SQL datum. Absent and explicit null are both
/// SQL NULL.
fn encode_value(enc: &mut DataRowEncoder, v: Option<&Bson>) -> PgWireResult<()> {
    // A timestamp CONSTANT never passes through a row, so it arrives here as a
    // BSON date or as the sub-millisecond composite rather than as something
    // `timestamp_text` reassembled. Without this arm it fell through to the
    // catch-all and `select '2026-01-01 12:00'::timestamp` answered NULL.
    if let Some(value) = v {
        if let Some(text) = secantus_pgplan::timestamp_value_text(value) {
            return enc.encode_field(&Some(text.as_str()));
        }
        // An interval is three parts in a document; the wire wants its text.
        if let Some(text) = secantus_pgplan::interval_value_text(value) {
            return enc.encode_field(&Some(text.as_str()));
        }
    }
    match v {
        Some(Bson::Int32(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::Int64(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::Double(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::Boolean(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::String(x)) => enc.encode_field(&Some(x.as_str())),
        // Decimal128's rendering already carries the scale (`1.50`, not `1.5`),
        // which is part of a PostgreSQL `numeric` value rather than formatting.
        Some(Bson::Decimal128(x)) => enc.encode_field(&Some(x.to_string().as_str())),
        // An array must be handed over as a TYPED vector, not as pre-rendered
        // text: `encode_field` encodes against the column's declared type, so
        // giving it a `&str` for an `int4[]` field wraps the whole literal as a
        // single element (`{1,2,3}` came out as `"{1,2,3}"`).
        // A nested array is REFUSED rather than flattened. rust-postgres
        // encodes only one dimension, so the typed path below silently turned
        // `{{1,2},{3,4}}` into two elements whose text was `{1,2}` and
        // `{3,4}` -- a wrong answer that a client cannot tell from a real one.
        // Guessing the client's format code to smuggle the literal through as
        // text would be the same trade in a less visible place.
        Some(Bson::Array(items)) if items.iter().any(|x| matches!(x, Bson::Array(_))) => {
            Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "0A000".to_owned(),
                "multidimensional arrays are not supported yet".to_owned(),
            ))))
        }
        Some(Bson::Array(items)) => match items.first() {
            Some(Bson::Int32(_)) => {
                let v: Vec<Option<i32>> = items.iter().map(|x| x.as_i32()).collect();
                enc.encode_field(&v)
            }
            Some(Bson::Int64(_)) => {
                let v: Vec<Option<i64>> = items.iter().map(|x| x.as_i64()).collect();
                enc.encode_field(&v)
            }
            Some(Bson::Double(_)) => {
                let v: Vec<Option<f64>> = items.iter().map(|x| x.as_f64()).collect();
                enc.encode_field(&v)
            }
            Some(Bson::Boolean(_)) => {
                let v: Vec<Option<bool>> = items.iter().map(|x| x.as_bool()).collect();
                enc.encode_field(&v)
            }
            // Text, decimals and an EMPTY array all go as strings; an empty
            // one has no element to read a type from and renders as `{}`
            // whatever the column type is.
            _ => {
                let v: Vec<Option<String>> = items
                    .iter()
                    .map(|x| match x {
                        Bson::Null => None,
                        Bson::String(t) => Some(t.clone()),
                        other => Some(render_array_element_text(other)),
                    })
                    .collect();
                enc.encode_field(&v)
            }
        },
        _ => enc.encode_field(&None::<i32>),
    }
}

/// The wire type an aggregate's result carries.
///
/// Probed against PostgreSQL 14: `count(*)` and `count(col)` are int8 (oid 20),
/// `sum(int4)` is **int8**, not int4, and `min`/`max` return the INPUT type.
fn aggregate_wire_type(item: &AggItem) -> Type {
    match item.func {
        AggFunc::CountStar | AggFunc::Count | AggFunc::Sum => Type::INT8,
        AggFunc::Min | AggFunc::Max => item
            .source_type
            .as_deref()
            .map(wire_type)
            .unwrap_or(Type::VARCHAR),
    }
}

/// One aggregate over one group.
///
/// PostgreSQL's NULL rules, probed on 14: `count(*)` counts ROWS; every other
/// aggregate SKIPS NULLs; and over an empty input `count` is 0 while `sum`,
/// `min` and `max` are **NULL, not zero**.
fn compute_aggregate(item: &AggItem, rows: &[Document]) -> Bson {
    let Some(field) = item.field.as_deref() else {
        // count(*)
        return Bson::Int64(rows.len() as i64);
    };
    let values: Vec<&Bson> = rows
        .iter()
        .filter_map(|d| match d.get(field) {
            None | Some(Bson::Null) => None,
            Some(v) => Some(v),
        })
        .collect();

    match item.func {
        AggFunc::CountStar => Bson::Int64(rows.len() as i64),
        AggFunc::Count => Bson::Int64(values.len() as i64),
        AggFunc::Sum => {
            if values.is_empty() {
                return Bson::Null;
            }
            // Integer inputs sum as int8; a float anywhere makes it a double.
            if values
                .iter()
                .all(|v| matches!(v, Bson::Int32(_) | Bson::Int64(_)))
            {
                let total: i64 = values
                    .iter()
                    .map(|v| match v {
                        Bson::Int32(x) => i64::from(*x),
                        Bson::Int64(x) => *x,
                        _ => 0,
                    })
                    .sum();
                Bson::Int64(total)
            } else {
                let total: f64 = values
                    .iter()
                    .map(|v| match v {
                        Bson::Int32(x) => f64::from(*x),
                        Bson::Int64(x) => *x as f64,
                        Bson::Double(x) => *x,
                        _ => 0.0,
                    })
                    .sum();
                Bson::Double(total)
            }
        }
        AggFunc::Min | AggFunc::Max => {
            let mut best: Option<&Bson> = None;
            for v in values {
                best = Some(match best {
                    None => v,
                    Some(cur) => {
                        let cmp = compare_values(v, cur);
                        let take = if item.func == AggFunc::Min {
                            cmp == Ordering::Less
                        } else {
                            cmp == Ordering::Greater
                        };
                        if take {
                            v
                        } else {
                            cur
                        }
                    }
                });
            }
            best.cloned().unwrap_or(Bson::Null)
        }
    }
}

/// Sort decoded rows in PostgreSQL's order.
///
/// Deliberately NOT pushed into the storage layer's sort: MongoDB orders null
/// LOW, while PostgreSQL puts NULLs LAST on ASC and FIRST on DESC (probed 14).
/// Pushing an ASC sort down would silently reorder every nullable column.
fn sort_rows(docs: &mut [Document], order: &[OrderKey]) {
    docs.sort_by(|a, b| {
        for key in order {
            let (l, r) = (a.get(&key.field), b.get(&key.field));
            let l_null = matches!(l, None | Some(Bson::Null));
            let r_null = matches!(r, None | Some(Bson::Null));
            let ord = match (l_null, r_null) {
                (true, true) => Ordering::Equal,
                (true, false) => match key.nulls {
                    Nulls::First => Ordering::Less,
                    Nulls::Last => Ordering::Greater,
                },
                (false, true) => match key.nulls {
                    Nulls::First => Ordering::Greater,
                    Nulls::Last => Ordering::Less,
                },
                (false, false) => {
                    let cmp = compare_values(l.unwrap(), r.unwrap());
                    if key.ascending {
                        cmp
                    } else {
                        cmp.reverse()
                    }
                }
            };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
}

/// Compare two non-null stored values the way PostgreSQL compares the SQL types
/// this slice supports. Numbers compare numerically across int/long/double,
/// strings byte-wise, booleans false < true.
fn compare_values(a: &Bson, b: &Bson) -> Ordering {
    fn as_f64(v: &Bson) -> Option<f64> {
        match v {
            Bson::Int32(i) => Some(f64::from(*i)),
            Bson::Int64(i) => Some(*i as f64),
            Bson::Double(d) => Some(*d),
            _ => None,
        }
    }
    match (a, b) {
        (Bson::String(x), Bson::String(y)) => x.cmp(y),
        (Bson::Boolean(x), Bson::Boolean(y)) => x.cmp(y),
        _ => match (as_f64(a), as_f64(b)) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            // Mixed or unsupported types cannot arise from one SQL column
            // today; keeping them equal is stable rather than arbitrary.
            _ => Ordering::Equal,
        },
    }
}

/// A parsed statement: the SQL text plus whatever parameter types the client
/// declared in `Parse`.
///
/// The text is kept rather than a plan, because the extended protocol binds
/// values AFTER parsing and the plan depends on them (`WHERE n = $1` lowers to
/// a different filter for a NULL than for a 5). Re-planning per Bind keeps one
/// set of NULL rules instead of two.
#[derive(Debug, Clone)]
pub struct ParsedStatement {
    pub sql: String,
    pub declared_types: Vec<Type>,
}

pub struct SqlParser;

#[async_trait]
impl QueryParser for SqlParser {
    type Statement = ParsedStatement;

    async fn parse_sql<C>(
        &self,
        _c: &C,
        sql: &str,
        types: &[Option<Type>],
    ) -> PgWireResult<Self::Statement>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        Ok(ParsedStatement {
            sql: sql.to_string(),
            // 0.40 reports an unspecified parameter as `None` rather than a
            // zero oid; both mean "the server decides", so flatten them.
            declared_types: types.iter().flatten().cloned().collect(),
        })
    }

    // These two exist so `ExtendedQueryHandler` can AUTO-implement Describe.
    // We override `do_describe_statement` / `do_describe_portal` explicitly --
    // resolving a result schema needs the catalog, which the parser has no
    // access to -- so the auto path is never taken and these stay empty.
    fn get_parameter_types(&self, stmt: &Self::Statement) -> PgWireResult<Vec<Type>> {
        Ok(stmt.declared_types.clone())
    }

    fn get_result_schema(
        &self,
        _stmt: &Self::Statement,
        _column_format: Option<&Format>,
    ) -> PgWireResult<Vec<FieldInfo>> {
        Ok(Vec::new())
    }
}

/// A PostgreSQL binary `numeric` as its exact decimal text.
///
/// Wire shape: `ndigits`, `weight`, `sign`, `dscale`, then `ndigits` base-10000
/// groups. The value is `sum(digits[i] * 10000^(weight - i))`, and `dscale` is
/// how many digits after the point to SHOW — which is part of the value here,
/// since `1.50` and `1.5` are different numerics.
///
/// Rendered back to text rather than computed into a float: a numeric carries
/// more digits than an f64 can hold, and the text path already knows how to
/// turn this into an exact value.
fn binary_numeric_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 8 {
        return None;
    }
    let be16 = |i: usize| i16::from_be_bytes([bytes[i], bytes[i + 1]]);
    let ndigits = be16(0);
    let weight = i32::from(be16(2));
    let sign = u16::from_be_bytes([bytes[4], bytes[5]]);
    let dscale = usize::from(u16::from_be_bytes([bytes[6], bytes[7]]));
    // 0xC000 is NaN; the infinities (0xD000 / 0xF000) arrived with PG 14.
    match sign {
        0xC000 => return Some("NaN".to_string()),
        0xD000 => return Some("Infinity".to_string()),
        0xF000 => return Some("-Infinity".to_string()),
        _ => {}
    }
    if ndigits < 0 || bytes.len() < 8 + (ndigits as usize) * 2 {
        return None;
    }
    let digits: Vec<i16> = (0..ndigits as usize).map(|i| be16(8 + i * 2)).collect();

    let mut out = String::new();
    if sign == 0x4000 {
        out.push('-');
    }
    if weight < 0 {
        out.push('0');
    } else {
        for i in 0..=weight as usize {
            let d = digits.get(i).copied().unwrap_or(0);
            if i == 0 {
                out.push_str(&d.to_string());
            } else {
                out.push_str(&format!("{d:04}"));
            }
        }
    }
    if dscale > 0 {
        out.push('.');
        let mut frac = String::new();
        let mut group = 1i32;
        while frac.len() < dscale {
            let idx = weight + group;
            let d = if idx >= 0 {
                digits.get(idx as usize).copied().unwrap_or(0)
            } else {
                0
            };
            frac.push_str(&format!("{d:04}"));
            group += 1;
        }
        frac.truncate(dscale);
        out.push_str(&frac);
    }
    Some(out)
}

/// One element of a PostgreSQL binary array, plus the reader position.
///
/// Wire shape: `ndim`, `has_null`, `element oid`, then per dimension a length
/// and a lower bound, then each element as a 4-byte length (-1 for NULL)
/// followed by that many bytes in the ELEMENT's binary format.
fn binary_array(bytes: &[u8], tz: &secantus_pgplan::TimeZoneSetting) -> PgWireResult<Bson> {
    if bytes.len() < 12 {
        return Err(unsupported_binary_oid(None));
    }
    let be32 = |i: usize| i32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let ndim = be32(0);
    let elem_oid = be32(8) as u32;
    if ndim == 0 {
        return Ok(Bson::Array(Vec::new()));
    }
    // Only one dimension: a nested array cannot be returned to a client here
    // either, so accepting one as a parameter would only move the wrong answer.
    if ndim != 1 {
        return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "0A000".into(),
            "multidimensional arrays are not supported yet".into(),
        ))));
    }
    let count = be32(12);
    let mut pos = 20; // 12 header + 8 for one dimension's length and lower bound
    let mut items = Vec::with_capacity(count.max(0) as usize);
    for _ in 0..count.max(0) {
        if pos + 4 > bytes.len() {
            return Err(unsupported_binary_oid(None));
        }
        let len = be32(pos);
        pos += 4;
        if len < 0 {
            items.push(Bson::Null);
            continue;
        }
        let end = pos + len as usize;
        if end > bytes.len() {
            return Err(unsupported_binary_oid(None));
        }
        let elem = Bytes::copy_from_slice(&bytes[pos..end]);
        let ty = Type::from_oid(elem_oid);
        items.push(decode_parameter(Some(&elem), ty.as_ref(), true, tz)?);
        pos = end;
    }
    Ok(Bson::Array(items))
}

fn unsupported_binary_oid(oid: Option<u32>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        "0A000".into(),
        format!("binary parameters of type oid {oid:?} are not supported yet"),
    )))
}

/// The element type behind an array oid, for the oids this server knows.
///
/// Used by BOTH parameter formats so an array parameter decodes to an array
/// either way.
fn element_of_array_oid(oid: u32) -> Option<&'static str> {
    Some(match oid {
        1000 => "bool",
        1005 => "int2",
        1007 => "int4",
        1009 => "text",
        1015 => "varchar",
        1016 => "int8",
        1021 => "float4",
        1022 => "float8",
        1231 => "numeric",
        1115 => "timestamp",
        1182 => "date",
        _ => return None,
    })
}

/// Decode one bound parameter into the value the planner will substitute.
///
/// `None` is SQL NULL. A client may declare a parameter's type as oid 0
/// ("unspecified") and leave the server to infer it, which is why the text path
/// falls back to sniffing the literal rather than assuming `text`.
fn decode_parameter(
    raw: Option<&Bytes>,
    ty: Option<&Type>,
    binary: bool,
    tz: &secantus_pgplan::TimeZoneSetting,
) -> PgWireResult<Bson> {
    let Some(bytes) = raw else {
        return Ok(Bson::Null);
    };
    if binary {
        // Binary format is width- and type-exact, so an unknown type here is a
        // genuine "cannot decode" rather than something to guess at.
        return match ty.map(|t| t.oid()) {
            Some(23) if bytes.len() == 4 => Ok(Bson::Int32(i32::from_be_bytes(
                bytes[..4].try_into().expect("checked"),
            ))),
            Some(20) if bytes.len() == 8 => Ok(Bson::Int64(i64::from_be_bytes(
                bytes[..8].try_into().expect("checked"),
            ))),
            Some(21) if bytes.len() == 2 => Ok(Bson::Int32(i32::from(i16::from_be_bytes(
                bytes[..2].try_into().expect("checked"),
            )))),
            Some(701) if bytes.len() == 8 => Ok(Bson::Double(f64::from_be_bytes(
                bytes[..8].try_into().expect("checked"),
            ))),
            Some(700) if bytes.len() == 4 => Ok(Bson::Double(f64::from(f32::from_be_bytes(
                bytes[..4].try_into().expect("checked"),
            )))),
            Some(16) if bytes.len() == 1 => Ok(Bson::Boolean(bytes[0] != 0)),
            Some(25) | Some(1043) | Some(19) | Some(1042) => {
                Ok(Bson::String(String::from_utf8_lossy(bytes).into_owned()))
            }
            // These decode to their CANONICAL TEXT so a binary parameter takes
            // exactly the same path through the planner as a text one -- the
            // text path already turns each of these into the right value, and
            // duplicating that here is how the two formats drift apart.
            Some(1700) => match binary_numeric_text(bytes) {
                Some(t) => secantus_pgplan::parse_numeric(&t)
                    .map(Bson::Decimal128)
                    .map_err(|e| PgHandler::err(&e)),
                None => Err(unsupported_binary_oid(Some(1700))),
            },
            Some(1082) if bytes.len() == 4 => {
                Ok(Bson::String(secantus_pgplan::render_date_from_pg_days(
                    i32::from_be_bytes(bytes[..4].try_into().expect("checked")),
                )))
            }
            Some(1083) if bytes.len() == 8 => {
                Ok(Bson::String(secantus_pgplan::render_time_from_micros(
                    i64::from_be_bytes(bytes[..8].try_into().expect("checked")),
                )))
            }
            // An instant on the wire, rendered into the session's zone -- the
            // same value a `::timestamptz` literal would have produced.
            Some(1184) if bytes.len() == 8 => {
                let pg_micros = i64::from_be_bytes(bytes[..8].try_into().expect("checked"));
                let micros = pg_micros + 946_684_800 * 1_000_000;
                Ok(Bson::String(secantus_pgplan::render_timestamptz(
                    micros, tz,
                )))
            }
            // `timetz` is 8 bytes of microseconds since midnight plus a 4-byte
            // offset in SECONDS WEST of UTC -- the opposite sign to the one the
            // text form prints, which is why this negates.
            Some(1266) if bytes.len() == 12 => {
                let us = i64::from_be_bytes(bytes[..8].try_into().expect("checked"));
                let west = i32::from_be_bytes(bytes[8..12].try_into().expect("checked"));
                Ok(Bson::String(secantus_pgplan::render_timetz(us, -west)))
            }
            // `interval`: 8 bytes of microseconds, then days, then months --
            // three parts on the wire for the same reason they are three parts
            // in the value, since neither converts without a calendar.
            Some(1186) if bytes.len() == 16 => {
                let micros = i64::from_be_bytes(bytes[..8].try_into().expect("checked"));
                let days = i32::from_be_bytes(bytes[8..12].try_into().expect("checked"));
                let months = i32::from_be_bytes(bytes[12..16].try_into().expect("checked"));
                Ok(secantus_pgplan::Interval {
                    months,
                    days,
                    micros,
                }
                .to_bson())
            }
            // `json` is UTF-8 text on the wire. `jsonb` is the same text
            // behind a one-byte format version, which is 1 and has been since
            // the type shipped -- an unknown version means the client is
            // speaking something this server has never seen, so it refuses
            // rather than guessing at the payload.
            Some(114) => Ok(Bson::String(String::from_utf8_lossy(bytes).into_owned())),
            Some(3802) => match bytes.split_first() {
                Some((1, rest)) => Ok(Bson::String(String::from_utf8_lossy(rest).into_owned())),
                _ => Err(unsupported_binary_oid(Some(3802))),
            },
            Some(1114) if bytes.len() == 8 => Ok(Bson::String(
                secantus_pgplan::render_timestamp_from_pg_micros(i64::from_be_bytes(
                    bytes[..8].try_into().expect("checked"),
                )),
            )),
            // Every array oid this server knows, decoded through the element's
            // own binary decoder rather than a per-type array reader.
            Some(oid) if element_of_array_oid(oid).is_some() => binary_array(bytes, tz),
            other => Err(unsupported_binary_oid(other)),
        };
    }

    let text = String::from_utf8_lossy(bytes);
    match ty.map(|t| t.oid()) {
        Some(23) | Some(21) => text
            .parse::<i32>()
            .map(Bson::Int32)
            .map_err(|_| invalid_text(&text, "integer")),
        Some(20) => text
            .parse::<i64>()
            .map(Bson::Int64)
            .map_err(|_| invalid_text(&text, "bigint")),
        Some(700) | Some(701) => text
            .parse::<f64>()
            .map(Bson::Double)
            .map_err(|_| invalid_text(&text, "double precision")),
        // A `numeric` parameter is EXACT, and was being parsed as an f64 --
        // so a client binding Decimal("0.1") got a float, and one binding
        // `1.50` lost the scale that makes it a different value from `1.5`.
        Some(1700) => secantus_pgplan::parse_numeric(&text)
            .map(Bson::Decimal128)
            .map_err(|e| PgHandler::err(&e)),
        Some(16) => Ok(Bson::Boolean(matches!(
            text.as_ref(),
            "t" | "true" | "TRUE" | "1" | "y" | "yes" | "on"
        ))),
        Some(25) | Some(1043) | Some(19) | Some(1042) => Ok(Bson::String(text.into_owned())),
        // The TYPED text forms. These reach the same value the BINARY path
        // produces for the same oid, which is the whole point: a parameter's
        // meaning cannot depend on the format a client happened to send it in.
        //
        // Left out, they fell through to `sniff_text` and became plain strings,
        // so `array[...] = %s` compared an array to a string and reported
        // "cannot compare" -- 98 failures whose message pointed at comparison
        // when the cause was here, one layer earlier.
        Some(oid @ (1082 | 1083 | 1114 | 1184 | 1266 | 1186 | 114 | 3802)) => {
            let target = match oid {
                1082 => "date",
                1083 => "time",
                1114 => "timestamp",
                1184 => "timestamptz",
                1266 => "timetz",
                114 => "json",
                3802 => "jsonb",
                _ => "interval",
            };
            secantus_pgplan::cast_text_to(&text, target, tz).map_err(|e| PgHandler::err(&e))
        }
        Some(oid) if element_of_array_oid(oid).is_some() => {
            let element = element_of_array_oid(oid).expect("checked");
            secantus_pgplan::cast_text_to(&text, &format!("{element}[]"), tz)
                .map_err(|e| PgHandler::err(&e))
        }
        // oid 0 = the client left the type to us. PostgreSQL infers from
        // context; sniffing the literal covers the shapes this server plans.
        _ => Ok(sniff_text(&text)),
    }
}

fn invalid_text(text: &str, want: &str) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".into(),
        "22P02".into(), // invalid_text_representation
        format!("invalid input syntax for type {want}: \"{text}\""),
    )))
}

fn sniff_text(text: &str) -> Bson {
    // Only treat it as a number when the number ROUND-TRIPS to the same text.
    // `01` parses as 1, but it is not how anyone writes 1 -- and a client that
    // sent `01` for an unspecified parameter may be sending JSON, where `01`
    // is invalid and must stay invalid. Sniffing must not make a value more
    // acceptable than the client wrote it.
    let round_trips = |rendered: String| rendered == text;
    if let Ok(i) = text.parse::<i32>() {
        if round_trips(i.to_string()) {
            return Bson::Int32(i);
        }
    }
    if let Ok(i) = text.parse::<i64>() {
        if round_trips(i.to_string()) {
            return Bson::Int64(i);
        }
    }
    if let Ok(f) = text.parse::<f64>() {
        if round_trips(f.to_string()) {
            return Bson::Double(f);
        }
    }
    Bson::String(text.to_string())
}

impl PgHandler {
    /// Every bound parameter of a portal, decoded in order.
    fn portal_params<S>(&self, portal: &Portal<S>) -> PgWireResult<Vec<Bson>>
    where
        S: Clone + Send + Sync,
    {
        let declared = &portal.statement.parameter_types;
        portal
            .parameters
            .iter()
            .enumerate()
            .map(|(i, raw)| {
                let binary = match &portal.parameter_format {
                    Format::UnifiedText => false,
                    Format::UnifiedBinary => true,
                    Format::Individual(codes) => codes.get(i).copied().unwrap_or(0) == 1,
                };
                // 0.40 stores an unspecified parameter type as `None`.
                decode_parameter(
                    raw.as_ref(),
                    declared.get(i).and_then(|t| t.as_ref()),
                    binary,
                    &self.session_timezone(),
                )
            })
            .collect()
    }

    /// The output columns a statement would produce, without running it.
    ///
    /// Planned against NULL placeholders: `Describe` arrives before `Bind`, so
    /// no values exist yet, and the result SHAPE does not depend on them.
    fn describe_fields(&self, sql: &str, n_params: usize) -> PgWireResult<Vec<FieldInfo>> {
        let params = vec![Bson::Null; n_params];
        let stmt = secantus_pgplan::plan_with_session(
            sql,
            &|n| self.lookup(n),
            &params,
            &self.session_timezone(),
        )
        .map_err(|e| Self::err(&e))?;
        Ok(match stmt {
            Statement::Select(sel) => {
                let def = self
                    .lookup(&sel.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(sel.table.clone())))?;
                sel.columns
                    .iter()
                    .map(|(out, _)| {
                        let ty = def
                            .column(out)
                            .map(|c| wire_type(&c.pg_type))
                            .unwrap_or(Type::VARCHAR);
                        FieldInfo::new(out.clone(), None, None, ty, FieldFormat::Text)
                    })
                    .collect()
            }
            Statement::Aggregate(agg) => {
                let def = self
                    .lookup(&agg.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(agg.table.clone())))?;
                agg.select
                    .iter()
                    .map(|(name, col)| {
                        let ty = match col {
                            OutputCol::Group(i) => def
                                .column(&agg.group_by[*i].0)
                                .map(|c| wire_type(&c.pg_type))
                                .unwrap_or(Type::VARCHAR),
                            OutputCol::Agg(i) => aggregate_wire_type(&agg.items[*i]),
                        };
                        FieldInfo::new(name.clone(), None, None, ty, FieldFormat::Text)
                    })
                    .collect()
            }
            Statement::Show(name) => vec![FieldInfo::new(
                canonical_setting(&name),
                None,
                None,
                Type::TEXT,
                FieldFormat::Text,
            )],
            Statement::SelectConstant(sc) => sc
                .columns
                .iter()
                .map(|(name, _, ty)| {
                    FieldInfo::new(name.clone(), None, None, wire_type(ty), FieldFormat::Text)
                })
                .collect(),
            // CREATE / INSERT / UPDATE / DELETE return no rows.
            _ => Vec::new(),
        })
    }
}

#[async_trait]
impl ExtendedQueryHandler for PgHandler {
    type Statement = ParsedStatement;
    type QueryParser = SqlParser;

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        Arc::new(SqlParser)
    }

    async fn do_describe_statement<C>(
        &self,
        _c: &mut C,
        target: &StoredStatement<Self::Statement>,
    ) -> PgWireResult<DescribeStatementResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let declared = &target.parameter_types;
        let fields = self.describe_fields(&target.statement.sql, declared.len())?;
        // An unspecified parameter (`None`) is reported to the client as
        // `unknown`, which is what PostgreSQL does when it cannot infer.
        let types: Vec<Type> = declared
            .iter()
            .map(|t| t.clone().unwrap_or(Type::UNKNOWN))
            .collect();
        Ok(DescribeStatementResponse::new(types, fields))
    }

    async fn do_describe_portal<C>(
        &self,
        _c: &mut C,
        target: &Portal<Self::Statement>,
    ) -> PgWireResult<DescribePortalResponse>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let fields = self.describe_fields(
            &target.statement.statement.sql,
            target.statement.parameter_types.len(),
        )?;
        Ok(DescribePortalResponse::new(fields))
    }

    async fn do_query<C>(
        &self,
        _c: &mut C,
        portal: &Portal<Self::Statement>,
        max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: pgwire::api::store::PortalStore<Statement = Self::Statement>,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let params = self.portal_params(portal)?;
        let mut responses = self
            .run(&portal.statement.statement.sql, &params, max_rows)
            .await?;
        // One portal is one statement, so exactly one response.
        Ok(responses.remove(0))
    }
}

/// Undo COPY's text-format escaping for one field.
///
/// PostgreSQL escapes the delimiter, newline and backslash itself, so a tab
/// inside a value arrives as `\\t` and must NOT be read as a field separator.
fn copy_unescape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Parse one COPY text field into the value its column stores.
///
/// `\N` is NULL -- distinct from the empty string, which is a real empty text
/// value. That distinction is the whole reason COPY has an escape at all.
fn copy_field(raw: &str, pg_type: &str) -> PgWireResult<Bson> {
    if raw == "\\N" {
        return Ok(Bson::Null);
    }
    let text = copy_unescape(raw);
    let bad = |want: &str| {
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "22P02".into(),
            format!("invalid input syntax for type {want}: \"{text}\""),
        )))
    };
    Ok(match pg_type {
        "int2" | "int4" | "integer" | "int" | "smallint" => {
            Bson::Int32(text.trim().parse().map_err(|_| bad("integer"))?)
        }
        "int8" | "bigint" => Bson::Int64(text.trim().parse().map_err(|_| bad("bigint"))?),
        "float4" | "float8" | "real" | "numeric" => {
            Bson::Double(text.trim().parse().map_err(|_| bad("double precision"))?)
        }
        "bool" | "boolean" => match text.trim() {
            "t" | "true" | "y" | "yes" | "on" | "1" => Bson::Boolean(true),
            "f" | "false" | "n" | "no" | "off" | "0" => Bson::Boolean(false),
            _ => return Err(bad("boolean")),
        },
        _ => Bson::String(text),
    })
}

#[async_trait]
impl CopyHandler for PgHandler {
    async fn on_copy_data<C>(&self, _c: &mut C, data: CopyData) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let mut guard = self.copy_in.lock().unwrap_or_else(|e| e.into_inner());
        // A chunk may split a row anywhere, so buffer and parse only at Done.
        match guard.as_mut() {
            Some(state) => state.buffer.extend_from_slice(&data.data),
            None => {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "57014".into(),
                    "COPY data arrived with no COPY in progress".into(),
                ))))
            }
        }
        Ok(())
    }

    async fn on_copy_done<C>(&self, client: &mut C, _done: CopyDone) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        let state = match self
            .copy_in
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            Some(s) => s,
            None => return Ok(()),
        };

        let text = String::from_utf8(state.buffer).map_err(|_| {
            PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".into(),
                "22021".into(), // character_not_in_repertoire
                "COPY data is not valid UTF-8".into(),
            )))
        })?;

        let mut docs = Vec::new();
        for line in text.split('\n') {
            // A trailing newline leaves an empty final line, and `\.` is the
            // end-of-data marker from the historical protocol.
            if line.is_empty() || line == "\\." {
                continue;
            }
            let raw: Vec<&str> = line.split('\t').collect();
            if raw.len() != state.fields.len() {
                return Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                    "ERROR".into(),
                    "22P04".into(), // bad_copy_file_format
                    format!(
                        "extra or missing columns for COPY (expected {}, got {})",
                        state.fields.len(),
                        raw.len()
                    ),
                ))));
            }
            let mut doc = Document::new();
            for ((field, ty), value) in state.fields.iter().zip(&state.types).zip(raw) {
                doc.insert(field.clone(), copy_field(value, ty)?);
            }
            docs.push(
                bson::to_vec(&doc)
                    .map_err(|e| Self::storage_err("could not encode a COPY row", e))?,
            );
        }

        let written = docs.len();
        if !docs.is_empty() {
            let (_, errors) = self
                .storage
                .insert(&self.db, &state.table, docs, true)
                .map_err(|e| Self::storage_err("could not insert COPY rows", e))?;
            if let Some(first) = errors.first() {
                let def = self
                    .lookup(&state.table)
                    .ok_or_else(|| Self::err(&PlanError::UndefinedTable(state.table.clone())))?;
                return Err(Self::write_error(&state.table, &def, first));
            }
        }

        // pgwire sends ReadyForQuery after this, but NOT CommandComplete --
        // without it the client waits for a result that never comes and
        // psycopg fails with "not enough values to unpack".
        client
            .feed(PgWireBackendMessage::CommandComplete(CommandComplete::new(
                format!("COPY {written}"),
            )))
            .await?;
        client.flush().await.map_err(PgWireError::from)?;
        Ok(())
    }

    async fn on_copy_fail<C>(&self, _c: &mut C, fail: CopyFail) -> PgWireError
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: std::fmt::Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        // The client abandoned the COPY: drop everything buffered rather than
        // insert a partial load.
        *self.copy_in.lock().unwrap_or_else(|e| e.into_inner()) = None;
        PgWireError::UserError(Box::new(ErrorInfo::new(
            "ERROR".into(),
            "57014".into(), // query_canceled
            format!("COPY from stdin failed: {}", fail.message),
        )))
    }
}

pub struct HandlerFactory(pub Arc<PgHandler>);

impl PgWireServerHandlers for HandlerFactory {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.0.clone()
    }
    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.0.clone()
    }
    fn copy_handler(&self) -> Arc<impl CopyHandler> {
        self.0.clone()
    }
    fn startup_handler(&self) -> Arc<impl pgwire::api::auth::StartupHandler> {
        self.0.clone()
    }
}
