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
use secantus_pgplan::{
    plan, AggFunc, AggItem, Error as PlanError, Nulls, OrderKey, OutputCol, Statement,
};
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
                        encode_value(&mut enc, d.get(f))?;
                    }
                    enc.finish()
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
                    enc.finish()
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
                        &bson::doc! { "$set": upd.set },
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

/// Encode one stored value as a SQL datum. Absent and explicit null are both
/// SQL NULL.
fn encode_value(enc: &mut DataRowEncoder, v: Option<&Bson>) -> PgWireResult<()> {
    match v {
        Some(Bson::Int32(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::Int64(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::Double(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::Boolean(x)) => enc.encode_field(&Some(*x)),
        Some(Bson::String(x)) => enc.encode_field(&Some(x.as_str())),
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

pub struct HandlerFactory(pub Arc<PgHandler>);

impl PgWireServerHandlers for HandlerFactory {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.0.clone()
    }
    fn startup_handler(&self) -> Arc<impl pgwire::api::auth::StartupHandler> {
        self.0.clone()
    }
}
