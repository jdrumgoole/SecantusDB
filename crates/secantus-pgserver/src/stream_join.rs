//! Joins read in bounded memory (batch 65).
//!
//! A join used to reach its consumer only as a fully materialised row set:
//! every side read whole (`join_rows` / `join_docs_with`), every joined row
//! built, and only then the outer select or aggregate run over them. Here
//! the common shape -- joins of stored tables, each right side small enough
//! to hold -- is read the other way round: every RIGHT side is read and
//! hashed first, within `join_inner_bytes` in all, and the leftmost table is
//! then read a batch at a time on the statement's own thread, each batch
//! joined, filtered and handed on. Memory is the hashed right sides plus a
//! batch; a right side past the bound declines BEFORE any row is handed on,
//! and the caller falls back to the materialised path.
//!
//! Everything runs while the statement runs, on its thread, so the ON and
//! WHERE expressions see the session state that thread has installed. A
//! select's joined rows go to `external_sort`'s runs (written to disk past
//! 16 MB), which pgwire then reads a batch per poll; an aggregate's go to
//! the bounded aggregates (`ungrouped_in_bounded_memory` /
//! `grouped_in_bounded_memory`).
//!
//! The rows, and their order, are the materialised path's: each left row in
//! scan order with its matching right rows in theirs, built by the same
//! code (`join_docs_core` / `join_rows_core`). Inside a transaction block
//! (batch 66) a stored table is read a batch at a time through the
//! block's own session (`scan_batch_after`), where its WHERE would be a
//! collection scan anyway. A subquery, function or LATERAL side, and a
//! right side past the bound (`grace_join`), stream too (batch 66). Not
//! with a row lock, under row-level security, or for a correlated
//! subquery's re-scan.

use super::*;

/// The bytes of right-side rows a streamed join holds in memory.
pub(crate) fn join_inner_bytes() -> usize {
    static BYTES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *BYTES.get_or_init(|| {
        std::env::var("SECANTUS_PG_JOIN_INNER_BYTES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64 << 20)
    })
}

/// `SECANTUS_PG_JOIN_STREAM=0` switches streamed joins off, so a test can
/// compare their rows with the materialised path's.
fn join_streaming_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("SECANTUS_PG_JOIN_STREAM").ok().as_deref() != Some("0"))
}

/// The rows a streamed join hands on at a time.
const JOIN_BATCH: usize = 256;

fn narrow_num(v: &Bson) -> Option<i64> {
    secantus_pgplan::regtype_oid(v)
        .or_else(|| secantus_pgplan::regclass_oid(v))
        .or(match v {
            Bson::Int32(x) => Some(i64::from(*x)),
            Bson::Int64(x) => Some(*x),
            _ => None,
        })
}

/// A float8 / numeric value, which a join compares by number with an int.
fn non_int_number(v: &Bson) -> bool {
    matches!(v, Bson::Double(_) | Bson::Decimal128(_))
        || matches!(v, Bson::Document(d) if d.contains_key(secantus_pgplan::numeric::WIDE_NUMERIC_KEY))
}

/// The narrow join's ON equality: numerically across int widths, a regtype
/// / regclass by its oid, a float8 / numeric by its value against any
/// number (batch 66: `float8_col = int_col` matched nothing -- the values
/// were compared structurally), anything else structurally.
pub(crate) fn narrow_eq(a: &Bson, b: &Bson) -> bool {
    if (non_int_number(a) || non_int_number(b))
        && (non_int_number(a) || narrow_num(a).is_some())
        && (non_int_number(b) || narrow_num(b).is_some())
    {
        return secantus_pgplan::compare_values(a, b) == Some(std::cmp::Ordering::Equal);
    }
    match (narrow_num(a), narrow_num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// A value's class under `narrow_eq`: two values with keys are equal
/// exactly when their keys are; a value with none (a double, a document
/// that is no oid ...) is compared by `narrow_eq` itself.
#[derive(Hash, PartialEq, Eq)]
enum NarrowKey {
    Num(i64),
    Str(String),
    Bool(bool),
    Null,
}

fn narrow_key(v: &Bson) -> Option<NarrowKey> {
    if let Some(n) = narrow_num(v) {
        return Some(NarrowKey::Num(n));
    }
    // An integral float8 is equal to the int of its value (`narrow_eq`).
    if let Bson::Double(x) = v {
        if x.fract() == 0.0 && x.abs() < 9.0e15 {
            return Some(NarrowKey::Num(*x as i64));
        }
    }
    match v {
        Bson::String(s) => Some(NarrowKey::Str(s.clone())),
        Bson::Boolean(b) => Some(NarrowKey::Bool(*b)),
        Bson::Null => Some(NarrowKey::Null),
        _ => None,
    }
}

/// The right rows of a narrow join by their ON value.
pub(crate) struct NarrowIndex {
    buckets: HashMap<NarrowKey, Vec<usize>>,
    unhashable: Vec<usize>,
}

impl NarrowIndex {
    pub(crate) fn new(rows: &[Document], field: &str) -> Self {
        let mut buckets: HashMap<NarrowKey, Vec<usize>> = HashMap::new();
        let mut unhashable = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            // A row without the field never matches.
            let Some(v) = r.get(field) else { continue };
            match narrow_key(v) {
                Some(k) => buckets.entry(k).or_default().push(i),
                None => unhashable.push(i),
            }
        }
        NarrowIndex {
            buckets,
            unhashable,
        }
    }

    /// The rows `narrow_eq` matches `value` to, in their order.
    pub(crate) fn matches(
        &self,
        rows: &[Document],
        field: &str,
        value: Option<&Bson>,
    ) -> Vec<usize> {
        let Some(value) = value else {
            return Vec::new();
        };
        let equal = |i: &usize| rows[*i].get(field).is_some_and(|r| narrow_eq(value, r));
        let mut out: Vec<usize> = match narrow_key(value) {
            Some(k) => self.buckets.get(&k).cloned().unwrap_or_default(),
            None => self
                .buckets
                .values()
                .flatten()
                .copied()
                .filter(|i| equal(i))
                .collect(),
        };
        out.extend(self.unhashable.iter().copied().filter(|i| equal(i)));
        out.sort_unstable();
        out
    }
}

/// A general join prepared to stream: its right sides read and held (or,
/// past the bound, partitioned to disk).
enum Prepared<'a> {
    /// A stored table read a batch at a time.
    Leaf {
        sel: &'a secantus_pgplan::Select,
        def: &'a TableDef,
        columns: &'a [(String, String)],
    },
    /// Any other leftmost item (a subquery, a function, a table whose WHERE
    /// a block reads through an index): its rows built whole when it is
    /// fed, as the materialised path builds them.
    Rows(&'a secantus_pgplan::joins::JoinNode),
    Join {
        node: &'a secantus_pgplan::joins::JoinNode,
        left: Box<Prepared<'a>>,
        right: Vec<Document>,
    },
    /// A right side past the bound (`grace_join`).
    Grace {
        node: &'a secantus_pgplan::joins::JoinNode,
        left: Box<Prepared<'a>>,
        right: crate::grace_join::GraceRight,
    },
    /// A LATERAL right side, run for each left batch's rows.
    Lateral {
        node: &'a secantus_pgplan::joins::JoinNode,
        left: Box<Prepared<'a>>,
    },
}

/// A narrow join's prepared right side.
enum NarrowRight {
    Held(Vec<Document>),
    Grace(crate::grace_join::GraceRight),
}

/// A narrow join row's key class (`NarrowIndex` buckets): a value with a
/// `narrow_key` partitions by it (NULL included -- the narrow join decides
/// NULL itself), any other is compared with every row.
fn narrow_class(d: &Document, field: &str) -> crate::grace_join::KeyClass {
    use crate::grace_join::KeyClass;
    match d.get(field) {
        None => KeyClass::Null,
        Some(v) => match narrow_key(v) {
            Some(NarrowKey::Num(n)) => KeyClass::Key(vec![format!("n{n}")]),
            Some(NarrowKey::Str(s)) => KeyClass::Key(vec![format!("s{s}")]),
            Some(NarrowKey::Bool(b)) => KeyClass::Key(vec![format!("b{b}")]),
            Some(NarrowKey::Null) => KeyClass::Key(vec!["z".into()]),
            None => KeyClass::Other,
        },
    }
}

/// A `Feed` hands a side's rows to its sink a batch at a time.
type Sink<'s> = &'s mut dyn FnMut(Vec<Document>) -> PgWireResult<bool>;

impl PgHandler {
    /// May a join stream at all here? Not inside a transaction (a side would
    /// read outside it), not for a locking select or a correlated
    /// subquery's re-scan, not under row-level security, and not while a
    /// DECLARE captures the rows.
    fn join_may_stream(&self) -> bool {
        join_streaming_enabled()
            && secantus_pgplan::with_scan_cache(|_| ()).is_none()
            && !LOCK_ROW_IDS.with(|c| c.get())
            && self.rls_enabled_docs().is_empty()
            && self
                .cursor_capture
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
    }

    /// A stored table this module reads a batch at a time: an ordinary
    /// table (not virtual, a sequence or temporary) of built-in types.
    fn stream_table(&self, table: &str) -> Option<TableDef> {
        if table.is_empty() || Self::virtual_table(table).is_some() {
            return None;
        }
        let def = self.lookup(table)?;
        let builtin = def
            .columns
            .iter()
            .all(|c| secantus_pgplan::pgtypes::oid_of_name(&c.pg_type).is_some());
        (builtin && !def.temp && def.column("is_called").is_none()).then_some(def)
    }

    /// A join leaf's select: one stored table, read as written.
    fn plain_leaf(&self, sel: &secantus_pgplan::Select) -> bool {
        sel.sub.is_none()
            && sel.join.is_none()
            && sel.series.is_none()
            && sel.windows.is_empty()
            && sel.residual.is_none()
            && sel.casts.iter().all(Option::is_none)
            && sel.order.is_empty()
            && sel.distinct == secantus_pgplan::Distinct::None
            && sel.limit.is_none()
            && sel.offset == 0
            && sel.lock.is_none()
            && sel.lock_clauses.is_empty()
            && self.stream_table(&sel.table).is_some()
    }

    /// A plain leaf this reads a batch at a time HERE: inside a transaction
    /// (`table_batches` then reads in RecordId order, through the block's
    /// session) only where `find_matching` would scan the collection too,
    /// so the order is the materialised path's.
    fn batched_leaf(&self, sel: &secantus_pgplan::Select) -> bool {
        self.plain_leaf(sel)
            && (!self.storage.in_user_txn()
                || sel.filter.is_empty()
                || matches!(
                    self.storage
                        .explain_plan(self.db(), &sel.table, &sel.filter),
                    Ok(secantus_storage::ExplainPlan::CollScan)
                ))
    }

    /// `table`'s rows matching `filter`, a batch at a time, in the order
    /// `find_matching` returns them; `false` from `sink` stops. Inside a
    /// transaction (a block, or a statement run apart from one) the rows
    /// are read on the current session, so they see its snapshot and its
    /// own writes (`scan_batch_after`).
    fn table_batches(
        &self,
        table: &str,
        filter: &Document,
        sink: &mut dyn FnMut(Vec<Vec<u8>>) -> PgWireResult<bool>,
    ) -> PgWireResult<()> {
        if self.storage.in_user_txn() {
            let mut after = None;
            loop {
                self.check_cancel()?;
                let (blobs, next) = self
                    .storage
                    .scan_batch_after(self.db(), table, filter, after, JOIN_BATCH)
                    .map_err(|e| Self::storage_err("could not read", e))?;
                if !sink(blobs)? || next.is_none() {
                    return Ok(());
                }
                after = next;
            }
        }
        let mut failed: Option<PgWireError> = None;
        let scanned =
            self.storage
                .scan_routed_batches(self.db(), table, filter, JOIN_BATCH, |blobs| {
                    if let Err(e) = self.check_cancel() {
                        failed = Some(e);
                        return false;
                    }
                    match sink(blobs) {
                        Ok(go) => go,
                        Err(e) => {
                            failed = Some(e);
                            false
                        }
                    }
                });
        if let Some(e) = failed {
            return Err(e);
        }
        scanned.map_err(|e| Self::storage_err("could not read", e))
    }

    /// A leaf's rows as `materialise_sub` builds them (each column through
    /// `resolve_cell`, keyed by the leaf def's fields) and then keyed by the
    /// joined row's keys, as `join_rows`' leaf arm does.
    fn leaf_docs(
        &self,
        sel: &secantus_pgplan::Select,
        leaf_def: &TableDef,
        columns: &[(String, String)],
        blobs: &[Vec<u8>],
    ) -> PgWireResult<Vec<Document>> {
        let table_def = self
            .lookup(&sel.table)
            .ok_or_else(|| Self::err(&PlanError::UndefinedTable(sel.table.clone())))?;
        let schema = self.row_schema(&table_def, &sel.columns, &sel.casts);
        let tz = self.session_timezone();
        let mut out = Vec::with_capacity(blobs.len());
        for b in blobs {
            let d = decode_doc(b).map_err(|e| Self::storage_err("could not decode a row", e))?;
            let mut row = Document::new();
            for (i, c) in leaf_def.columns.iter().enumerate() {
                let v = match sel.columns.get(i) {
                    Some((_, field)) => resolve_cell(&d, field, None, schema[i].datatype(), &tz)?,
                    None => None,
                };
                row.insert(c.field(), v.unwrap_or(Bson::Null));
            }
            let mut keyed = Document::new();
            for (key, field) in columns {
                keyed.insert(key.clone(), row.get(field).cloned().unwrap_or(Bson::Null));
            }
            out.push(keyed);
        }
        Ok(out)
    }

    /// Read every right side of `node`'s left spine, within `budget` bytes
    /// in all (a stored table past it is partitioned to disk instead);
    /// `None` when the shape is not one this streams or a right side that
    /// cannot be partitioned is past the bound.
    fn prepare_join<'a>(
        &self,
        node: &'a secantus_pgplan::joins::JoinNode,
        budget: &mut usize,
    ) -> PgWireResult<Option<Prepared<'a>>> {
        use secantus_pgplan::joins::JoinNode;
        match node {
            JoinNode::Leaf { plan, def, columns } => match plan.as_ref() {
                Statement::Select(sel) if self.batched_leaf(sel) => {
                    Ok(Some(Prepared::Leaf { sel, def, columns }))
                }
                _ => Ok(Some(Prepared::Rows(node))),
            },
            JoinNode::Lateral { .. } => Ok(None),
            JoinNode::Join {
                left, right, equi, ..
            } => {
                let Some(left) = self.prepare_join(left, budget)? else {
                    return Ok(None);
                };
                let left = Box::new(left);
                if let JoinNode::Lateral { .. } = right.as_ref() {
                    return Ok(Some(Prepared::Lateral { node, left }));
                }
                // A stored table: read a batch at a time, partitioned to
                // disk if it passes the bound.
                if let JoinNode::Leaf { plan, def, columns } = right.as_ref() {
                    if let Statement::Select(rsel) = plan.as_ref() {
                        if self.batched_leaf(rsel) {
                            let rk: Vec<&String> = equi.iter().map(|(_, r)| r).collect();
                            let mut held: Vec<Document> = Vec::new();
                            let mut held_bytes = 0usize;
                            let mut grace: Option<crate::grace_join::GraceRight> = None;
                            let mut declined = false;
                            self.table_batches(&rsel.table, &rsel.filter, &mut |batch| {
                                let bytes: usize = batch.iter().map(Vec::len).sum();
                                let docs = self.leaf_docs(rsel, def, columns, &batch)?;
                                if grace.is_none() && bytes <= *budget {
                                    *budget -= bytes;
                                    held_bytes += bytes;
                                    held.extend(docs);
                                    return Ok(true);
                                }
                                if equi.is_empty() {
                                    declined = true;
                                    return Ok(false);
                                }
                                if grace.is_none() {
                                    // The rows held so far go to disk too.
                                    *budget += std::mem::take(&mut held_bytes);
                                }
                                let g = grace.get_or_insert_with(|| {
                                    crate::grace_join::GraceRight::new(*budget)
                                });
                                if !held.is_empty() && !g.add(std::mem::take(&mut held), &|d| crate::grace_join::general_key(d, &rk))? {
                                    declined = true;
                                    return Ok(false);
                                }
                                if !g.add(docs, &|d| crate::grace_join::general_key(d, &rk))? {
                                    declined = true;
                                    return Ok(false);
                                }
                                Ok(true)
                            })?;
                            if declined {
                                return Ok(None);
                            }
                            return Ok(Some(match grace {
                                Some(right) => Prepared::Grace { node, left, right },
                                None => Prepared::Join {
                                    node,
                                    left,
                                    right: held,
                                },
                            }));
                        }
                    }
                }
                // Anything else (a subquery, a function, a join): built
                // whole, as the materialised path builds it, and held only
                // within the bound.
                let rows = self.join_rows(right)?;
                let bytes: usize = rows.iter().map(doc_bytes).sum();
                if bytes > *budget {
                    return Ok(None);
                }
                *budget -= bytes;
                Ok(Some(Prepared::Join {
                    node,
                    left,
                    right: rows,
                }))
            }
        }
    }

    /// Hand a prepared join's rows to `sink` a batch at a time.
    fn feed_join(&self, prepared: Prepared<'_>, sink: Sink<'_>) -> PgWireResult<()> {
        use secantus_pgplan::joins::JoinNode;
        match prepared {
            Prepared::Leaf { sel, def, columns } => {
                self.table_batches(&sel.table, &sel.filter, &mut |blobs| {
                    let docs = self.leaf_docs(sel, def, columns, &blobs)?;
                    sink(docs)
                })
            }
            Prepared::Rows(node) => {
                let rows = self.join_rows(node)?;
                for chunk in rows.chunks(JOIN_BATCH) {
                    if !sink(chunk.to_vec())? {
                        break;
                    }
                }
                Ok(())
            }
            Prepared::Lateral { node, left } => {
                let JoinNode::Join {
                    kind,
                    right,
                    on,
                    merged,
                    left_keys,
                    right_keys,
                    ..
                } = node
                else {
                    return Err(Self::err(&PlanError::Internal("a prepared join".into())));
                };
                let JoinNode::Lateral {
                    sql,
                    params,
                    keys,
                    columns,
                } = right.as_ref()
                else {
                    return Err(Self::err(&PlanError::Internal("a LATERAL side".into())));
                };
                // The item's rows per distinct left key, as the materialised
                // path memoises them -- dropped when they pass the bound.
                let mut cache: HashMap<String, Vec<Document>> = HashMap::new();
                let mut cached = 0usize;
                self.feed_join(*left, &mut |lrows| {
                    let before = cache.len();
                    let out = self.lateral_rows(
                        &mut cache,
                        &lrows,
                        sql,
                        params,
                        keys,
                        columns,
                        *kind,
                        on.as_ref(),
                        merged,
                        left_keys,
                        right_keys,
                    )?;
                    if cache.len() != before {
                        cached = cache.values().flatten().map(doc_bytes).sum();
                    }
                    if cached > join_inner_bytes() {
                        cache.clear();
                        cached = 0;
                    }
                    sink(out)
                })
            }
            Prepared::Grace { node, left, right } => {
                let JoinNode::Join {
                    kind,
                    on,
                    equi,
                    merged,
                    left_keys,
                    right_keys,
                    ..
                } = node
                else {
                    return Err(Self::err(&PlanError::Internal("a prepared join".into())));
                };
                let mut lk = left_keys.clone();
                lk.push(crate::grace_join::LSEQ.to_string());
                let mut rk = right_keys.clone();
                rk.push(crate::grace_join::RSEQ.to_string());
                let spec = crate::grace_join::JoinSpec {
                    kind: *kind,
                    on: on.as_ref(),
                    equi,
                    merged,
                    left_keys: lk,
                    right_keys: rk,
                };
                use crate::grace_join::{general_key, GraceOps};
                let lkeys: Vec<&String> = equi.iter().map(|(l, _)| l).collect();
                let rkeys: Vec<&String> = equi.iter().map(|(_, r)| r).collect();
                let left_key = |d: &Document| general_key(d, &lkeys);
                let right_key = |d: &Document| general_key(d, &rkeys);
                let join = |rrows: Vec<Document>,
                            feed: &mut dyn FnMut(Sink<'_>) -> PgWireResult<()>,
                            emit: Sink<'_>,
                            as_left: bool|
                 -> PgWireResult<()> {
                    let kind = if as_left {
                        secantus_pgplan::joins::JoinKind::Left
                    } else {
                        spec.kind
                    };
                    self.join_rows_core(
                        kind,
                        spec.on,
                        spec.equi,
                        spec.merged,
                        &spec.left_keys,
                        &spec.right_keys,
                        feed,
                        rrows,
                        emit,
                    )
                };
                use secantus_pgplan::joins::JoinKind;
                let ops = GraceOps {
                    left_key: &left_key,
                    right_key: &right_key,
                    join: &join,
                    outer_left: matches!(kind, JoinKind::Left | JoinKind::Full),
                    outer_right: matches!(kind, JoinKind::Right | JoinKind::Full),
                };
                let mut left = Some(*left);
                self.grace_join(
                    &ops,
                    right,
                    &mut |inner_sink| match left.take() {
                        Some(l) => self.feed_join(l, inner_sink),
                        None => Ok(()),
                    },
                    sink,
                )
            }
            Prepared::Join { node, left, right } => {
                let JoinNode::Join {
                    kind,
                    on,
                    equi,
                    merged,
                    left_keys,
                    right_keys,
                    ..
                } = node
                else {
                    return Err(Self::err(&PlanError::Internal("a prepared join".into())));
                };
                let mut left = Some(*left);
                self.join_rows_core(
                    *kind,
                    on.as_ref(),
                    equi,
                    merged,
                    left_keys,
                    right_keys,
                    &mut |inner_sink| match left.take() {
                        Some(l) => self.feed_join(l, inner_sink),
                        None => Ok(()),
                    },
                    right,
                    sink,
                )
            }
        }
    }

    /// A general join (`Statement::JoinRows`) prepared to stream, or `None`.
    fn prepared_join_rows<'a>(&self, plan: &'a Statement) -> PgWireResult<Option<Prepared<'a>>> {
        let Statement::JoinRows(join) = plan else {
            return Ok(None);
        };
        if !matches!(join.tree, secantus_pgplan::joins::JoinNode::Join { .. })
            || !self.join_may_stream()
        {
            return Ok(None);
        }
        let mut budget = join_inner_bytes();
        self.prepare_join(&join.tree, &mut budget)
    }

    /// A narrow join (`JoinSelect`) prepared to stream: its right side
    /// held -- a stored table's rows, partitioned to disk past the bound,
    /// or a subquery's built whole within it -- or `None`.
    fn prepared_narrow(
        &self,
        join: &secantus_pgplan::JoinSelect,
    ) -> PgWireResult<Option<NarrowRight>> {
        let table_or_sub = |sub: &Option<Box<Statement>>, table: &str| {
            sub.is_some() || self.stream_table(table).is_some()
        };
        if join.order.is_some()
            || !self.join_may_stream()
            || !table_or_sub(&join.left_sub, &join.left.0)
            || !table_or_sub(&join.right_sub, &join.right.0)
        {
            return Ok(None);
        }
        let mut budget = join_inner_bytes();
        if let Some(stmt) = join.right_sub.as_ref() {
            let rows = self.sub_plan_rows(stmt)?;
            let bytes: usize = rows.iter().map(doc_bytes).sum();
            return Ok((bytes <= budget).then_some(NarrowRight::Held(rows)));
        }
        let right_key = self.narrow_on_fields(join).map(|(_, r)| r);
        let mut held: Vec<Document> = Vec::new();
        let mut grace: Option<crate::grace_join::GraceRight> = None;
        let mut declined = false;
        let key = |d: &Document| narrow_class(d, right_key.as_deref().unwrap_or(""));
        self.table_batches(&join.right.0, &Document::new(), &mut |batch| {
            let bytes: usize = batch.iter().map(Vec::len).sum();
            let docs = batch
                .iter()
                .map(|b| decode_doc(b))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| Self::storage_err("could not decode a row", e))?;
            if grace.is_none() && bytes <= budget {
                budget -= bytes;
                held.extend(docs);
                return Ok(true);
            }
            if right_key.is_none() {
                declined = true;
                return Ok(false);
            }
            let g = grace.get_or_insert_with(|| {
                crate::grace_join::GraceRight::new(join_inner_bytes())
            });
            if !held.is_empty() && !g.add(std::mem::take(&mut held), &key)? {
                declined = true;
                return Ok(false);
            }
            if !g.add(docs, &key)? {
                declined = true;
                return Ok(false);
            }
            Ok(true)
        })?;
        if declined {
            return Ok(None);
        }
        Ok(Some(match grace {
            Some(g) => NarrowRight::Grace(g),
            None => NarrowRight::Held(held),
        }))
    }

    /// The stored fields a narrow join's ON compares, (left, right), as
    /// `join_docs_core` resolves them; `None` for a cross join.
    fn narrow_on_fields(&self, join: &secantus_pgplan::JoinSelect) -> Option<(String, String)> {
        let ((la, lc), (_, rc)) = match join.on.as_ref()? {
            (a, b) if a.0 == join.left.1 => (a, b),
            (a, b) => (b, a),
        };
        let _ = la;
        let field = |sub: &Option<Box<Statement>>, table: &str, col: &str| {
            if sub.is_some() {
                Some(col.to_string())
            } else {
                self.lookup(table).and_then(|d| d.field_of(col))
            }
        };
        Some((
            field(&join.left_sub, &join.left.0, lc)?,
            field(&join.right_sub, &join.right.0, rc)?,
        ))
    }

    /// Hand a narrow join's rows to `sink` a batch at a time.
    fn feed_narrow(
        &self,
        join: &secantus_pgplan::JoinSelect,
        right: NarrowRight,
        sink: Sink<'_>,
    ) -> PgWireResult<()> {
        let mut feed_left = |inner_sink: Sink<'_>| -> PgWireResult<()> {
            if let Some(stmt) = join.left_sub.as_ref() {
                let rows = self.sub_plan_rows(stmt)?;
                for chunk in rows.chunks(JOIN_BATCH) {
                    if !inner_sink(chunk.to_vec())? {
                        break;
                    }
                }
                return Ok(());
            }
            self.table_batches(&join.left.0, &Document::new(), &mut |blobs| {
                let docs = blobs
                    .iter()
                    .map(|b| decode_doc(b))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| Self::storage_err("could not decode a row", e))?;
                inner_sink(docs)
            })
        };
        match right {
            NarrowRight::Held(rows) => self.join_docs_core(join, &mut feed_left, rows, sink),
            NarrowRight::Grace(g) => {
                let (lf, rf) = self
                    .narrow_on_fields(join)
                    .ok_or_else(|| Self::err(&PlanError::Internal("a narrow ON".into())))?;
                let left_key = |d: &Document| narrow_class(d, &lf);
                let right_key = |d: &Document| narrow_class(d, &rf);
                let run = |rrows: Vec<Document>,
                           feed: &mut dyn FnMut(Sink<'_>) -> PgWireResult<()>,
                           emit: Sink<'_>,
                           _as_left: bool|
                 -> PgWireResult<()> { self.join_docs_core(join, feed, rrows, emit) };
                let ops = crate::grace_join::GraceOps {
                    left_key: &left_key,
                    right_key: &right_key,
                    join: &run,
                    outer_left: join.left_join,
                    outer_right: false,
                };
                self.grace_join(&ops, g, &mut feed_left, sink)
            }
        }
    }

    /// A SELECT over a join, streamed: the joined rows are built now, on
    /// this thread, through the outer WHERE, into sorted runs (no ORDER BY:
    /// one run in arrival order), which the response then reads a batch per
    /// poll. `None` where the materialised path applies.
    pub(crate) fn try_stream_join_select(
        &self,
        sel: &secantus_pgplan::Select,
        env: &RowEnv,
    ) -> PgWireResult<Option<Response>> {
        let plain_outer = sel.windows.is_empty()
            && sel.series.is_none()
            && sel.casts.iter().all(Option::is_none)
            && sel.distinct == secantus_pgplan::Distinct::None
            && sel.order.iter().all(|k| k.expr.is_none())
            && sel.offset >= 0
            && sel.lock.is_none()
            && sel.lock_clauses.is_empty();
        if !plain_outer {
            return Ok(None);
        }
        enum Source<'a> {
            General(Prepared<'a>),
            Narrow(&'a secantus_pgplan::JoinSelect, NarrowRight),
        }
        let (source, def) = if let Some(sub) = sel.sub.as_ref() {
            match self.prepared_join_rows(&sub.plan)? {
                Some(p) => (Source::General(p), sub.def.clone()),
                None => return Ok(None),
            }
        } else if let Some(join) = sel.join.as_ref() {
            if !sel.filter.is_empty() || sel.residual.is_some() || !sel.order.is_empty() {
                return Ok(None);
            }
            match self.prepared_narrow(join)? {
                Some(right) => {
                    let def = secantus_pgplan::join_output_def(join, &|n| self.lookup(n))
                        .map_err(|e| Self::err(&e))?;
                    (Source::Narrow(join, right), def)
                }
                None => return Ok(None),
            }
        } else {
            return Ok(None);
        };
        let skip = usize::try_from(sel.offset).unwrap_or(0);
        let limit = sel
            .limit
            .map(|l| usize::try_from(l.max(0)).unwrap_or(usize::MAX));
        let cap = limit.map(|l| l.saturating_add(skip));
        let top_k = if sel.order.is_empty() {
            None
        } else {
            cap.filter(|c| *c <= 100_000)
        };
        let mut runs = crate::external_sort::RunBuilder::new(&sel.order, top_k, JOIN_BATCH);
        let mut pushed = 0usize;
        let empty = Document::new();
        let in_sets = secantus_core::query::InSets::prepare(&sel.filter);
        let _in_sets = secantus_core::query::InSetsGuard::install(&in_sets);
        let mut sink = |docs: Vec<Document>| -> PgWireResult<bool> {
            self.check_cancel()?;
            for d in docs {
                if !sel.filter.is_empty()
                    && !secantus_core::query::matches(&d, &sel.filter, &empty, None)
                        .unwrap_or(false)
                {
                    continue;
                }
                if let Some(residual) = sel.residual.as_ref() {
                    let v =
                        secantus_pgplan::apply_row_expr(residual, &d).map_err(|e| Self::err(&e))?;
                    if v != Bson::Boolean(true) {
                        continue;
                    }
                }
                let bytes = bson::to_vec(&d).map_or(0, |b| b.len());
                runs.push(d, bytes)
                    .map_err(|e| Self::user_error("XX000", format!("could not sort: {e}")))?;
                pushed += 1;
            }
            // Without an ORDER BY the first OFFSET + LIMIT rows are the
            // answer: stop reading.
            Ok(!(sel.order.is_empty() && cap.is_some_and(|c| pushed >= c)))
        };
        match source {
            Source::General(p) => self.feed_join(p, &mut sink)?,
            Source::Narrow(join, right) => self.feed_narrow(join, right, &mut sink)?,
        }
        drop(sink);
        let sorted = runs
            .finish(skip, limit, None)
            .map_err(|e| Self::user_error("XX000", format!("could not sort: {e}")))?;
        let schema = Arc::new(self.row_schema(&def, &sel.columns, &sel.casts));
        let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
        let source = self.sorted_stream(sorted);
        let source = source.flat_map(|item| match item {
            Ok(docs) => stream::iter(docs.into_iter().map(Ok)).boxed(),
            Err(e) => stream::iter(vec![Err(e)]).boxed(),
        });
        self.streamed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(Some(Response::Query(self.project_stream(
            source.boxed(),
            schema,
            fields,
            sel.casts.clone(),
            env,
        ))))
    }

    /// An aggregate's input when it is a join this streams: the joined rows
    /// through the aggregate's WHERE, each batch encoded for the bounded
    /// aggregates' sink. `None` where the materialised path applies.
    pub(crate) fn join_aggregate_source<'a>(
        &self,
        agg: &'a secantus_pgplan::Aggregate,
    ) -> PgWireResult<Option<JoinFeed<'a>>> {
        let Some(sub) = agg.sub.as_ref() else {
            return Ok(None);
        };
        Ok(self.prepared_join_rows(&sub.plan)?.map(|p| JoinFeed {
            prepared: Some(p),
            filter: &agg.filter,
        }))
    }

    /// Run a [`JoinFeed`], handing its rows on as documents.
    pub(crate) fn run_join_feed(
        &self,
        feed: &mut JoinFeed<'_>,
        sink: &mut dyn FnMut(RowBatch) -> bool,
    ) -> PgWireResult<()> {
        let Some(prepared) = feed.prepared.take() else {
            return Ok(());
        };
        let filter = feed.filter;
        let empty = Document::new();
        let in_sets = secantus_core::query::InSets::prepare(filter);
        let _in_sets = secantus_core::query::InSetsGuard::install(&in_sets);
        self.feed_join(prepared, &mut |mut docs| {
            if !filter.is_empty() {
                docs.retain(|d| {
                    secantus_core::query::matches(d, filter, &empty, None).unwrap_or(false)
                });
            }
            Ok(sink(RowBatch::Docs(docs)))
        })
    }
}

/// A join prepared as an aggregate's input (see `join_aggregate_source`).
pub(crate) struct JoinFeed<'a> {
    prepared: Option<Prepared<'a>>,
    filter: &'a Document,
}

/// Rows handed to the bounded aggregates: stored blobs from a table scan,
/// or documents already built (a join's rows -- encoding them only to
/// decode them again cost batch 65 a fifth of an aggregate over a join).
pub(crate) enum RowBatch {
    Blobs(Vec<Vec<u8>>),
    Docs(Vec<Document>),
}

impl RowBatch {
    /// The documents and their total size in bytes (a blob's length, or
    /// [`doc_bytes`] for a document).
    pub(crate) fn into_docs(self) -> PgWireResult<(Vec<Document>, usize)> {
        let rows = self.into_sized_docs()?;
        let n = rows.iter().map(|(_, n)| n).sum();
        Ok((rows.into_iter().map(|(d, _)| d).collect(), n))
    }

    /// Each document with its size in bytes.
    pub(crate) fn into_sized_docs(self) -> PgWireResult<Vec<(Document, usize)>> {
        match self {
            RowBatch::Blobs(blobs) => blobs
                .iter()
                .map(|b| {
                    decode_doc(b)
                        .map(|d| (d, b.len()))
                        .map_err(|e| PgHandler::storage_err("could not decode a row", e))
                })
                .collect(),
            RowBatch::Docs(docs) => Ok(docs
                .into_iter()
                .map(|d| {
                    let n = doc_bytes(&d);
                    (d, n)
                })
                .collect()),
        }
    }
}

/// About the BSON size of `d`, without encoding it.
pub(crate) fn doc_bytes(d: &Document) -> usize {
    fn value(v: &Bson) -> usize {
        match v {
            Bson::String(s) => s.len() + 5,
            Bson::Document(d) => doc_bytes(d),
            Bson::Array(a) => 5 + a.iter().map(|v| 4 + value(v)).sum::<usize>(),
            Bson::Binary(b) => b.bytes.len() + 5,
            Bson::Decimal128(_) => 16,
            Bson::RegularExpression(r) => r.pattern.len() + r.options.len() + 2,
            Bson::JavaScriptCode(c) => c.len() + 5,
            Bson::Symbol(s) => s.len() + 5,
            _ => 8,
        }
    }
    5 + d.iter().map(|(k, v)| k.len() + 2 + value(v)).sum::<usize>()
}
