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

use std::sync::Arc;

use async_trait::async_trait;
use bson::{Bson, Document};
use futures::{stream, Sink, StreamExt};
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::query::SimpleQueryHandler;
use pgwire::api::results::{DataRowEncoder, FieldFormat, FieldInfo, QueryResponse, Response, Tag};
use pgwire::api::{ClientInfo, PgWireServerHandlers, Type};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use secantus_pgcatalog::{TableDef, CATALOG_COLLECTION};
use secantus_pgplan::{plan, Error as PlanError, Statement};
use secantus_storage::Storage;

/// One database's worth of SQL over a shared `Storage`.
pub struct PgHandler {
    storage: Arc<Storage>,
    db: String,
}

impl PgHandler {
    pub fn new(storage: Arc<Storage>, db: &str) -> Self {
        Self {
            storage,
            db: db.to_string(),
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
            // LIMITATION (pgwire 0.31): `ErrorInfo` exposes severity/code/
            // message/detail/hint/position/where/file/line/routine, but NOT the
            // protocol's schema/table/column/constraint fields. Real PostgreSQL
            // sends `constraint_name = "<table>_pkey"` and `table_name` here,
            // and pgjdbc surfaces them as
            // `PSQLException.getServerErrorMessage().getConstraint()`. Tracked
            // in tasks/rust-pgserver-plan.md; either upstream the fields or
            // hand-roll the codec if a gauge starts asserting them.
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

/// The PostgreSQL type a column's declared type maps onto over the wire.
fn wire_type(pg_type: &str) -> Type {
    match pg_type {
        "int2" => Type::INT2,
        "int4" | "integer" | "int" => Type::INT4,
        "int8" | "bigint" => Type::INT8,
        "float4" => Type::FLOAT4,
        "float8" => Type::FLOAT8,
        "bool" | "boolean" => Type::BOOL,
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
    async fn do_query<'a, C>(&self, _c: &mut C, query: &str) -> PgWireResult<Vec<Response<'a>>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let sql = query.trim().trim_end_matches(';').trim();
        if sql.is_empty() {
            return Ok(vec![Response::EmptyQuery]);
        }

        let stmt = plan(sql, &|n| self.lookup(n)).map_err(|e| Self::err(&e))?;
        match stmt {
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
                let rows = stream::iter(raw).map(move |bytes| {
                    let d: Document = bson::from_slice(&bytes).map_err(|e| {
                        PgWireError::ApiError(Box::new(std::io::Error::other(e.to_string())))
                    })?;
                    let mut enc = DataRowEncoder::new(schema_ref.clone());
                    for f in &fields {
                        match d.get(f) {
                            Some(Bson::Int32(v)) => enc.encode_field(&Some(*v))?,
                            Some(Bson::Int64(v)) => enc.encode_field(&Some(*v))?,
                            Some(Bson::Double(v)) => enc.encode_field(&Some(*v))?,
                            Some(Bson::Boolean(v)) => enc.encode_field(&Some(*v))?,
                            Some(Bson::String(v)) => enc.encode_field(&Some(v.as_str()))?,
                            // Absent and explicit null are both SQL NULL here.
                            _ => enc.encode_field(&None::<i32>)?,
                        }
                    }
                    enc.finish()
                });
                Ok(vec![Response::Query(QueryResponse::new(schema, rows))])
            }
        }
    }
}

pub struct HandlerFactory(pub Arc<PgHandler>);

impl PgWireServerHandlers for HandlerFactory {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.0.clone()
    }
    fn startup_handler(&self) -> Arc<impl pgwire::api::auth::StartupHandler> {
        self.0.clone()
    }
}
