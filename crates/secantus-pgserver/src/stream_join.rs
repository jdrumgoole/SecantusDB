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
//! code (`join_docs_core` / `join_rows_core`). Only outside a transaction
//! block (each side then reads as the materialised path does, by its own
//! read), with no row lock, and not for a correlated subquery's re-scan.

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

/// The narrow join's ON equality: numerically across int widths, a regtype
/// / regclass by its oid, anything else structurally.
pub(crate) fn narrow_eq(a: &Bson, b: &Bson) -> bool {
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

/// A general join prepared to stream: its right sides read and held.
enum Prepared<'a> {
    Leaf {
        sel: &'a secantus_pgplan::Select,
        def: &'a TableDef,
        columns: &'a [(String, String)],
    },
    Join {
        node: &'a secantus_pgplan::joins::JoinNode,
        left: Box<Prepared<'a>>,
        right: Vec<Document>,
    },
}

/// A `Feed` hands a side's rows to its sink a batch at a time.
type Sink<'s> = &'s mut dyn FnMut(Vec<Document>) -> PgWireResult<bool>;

impl PgHandler {
    /// May a join stream at all here? Not inside a transaction (a side would
    /// read outside it), not for a locking select or a correlated
    /// subquery's re-scan, not under row-level security, and not while a
    /// DECLARE captures the rows.
    fn join_may_stream(&self) -> bool {
        !self.storage.in_user_txn()
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

    /// `table`'s rows matching `filter`, a batch at a time, in the order
    /// `find_matching` returns them; `false` from `sink` stops.
    fn table_batches(
        &self,
        table: &str,
        filter: &Document,
        sink: &mut dyn FnMut(Vec<Vec<u8>>) -> PgWireResult<bool>,
    ) -> PgWireResult<()> {
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
    /// in all; `None` when the shape is not one this streams or a right side
    /// is past the bound.
    fn prepare_join<'a>(
        &self,
        node: &'a secantus_pgplan::joins::JoinNode,
        budget: &mut usize,
    ) -> PgWireResult<Option<Prepared<'a>>> {
        use secantus_pgplan::joins::JoinNode;
        match node {
            JoinNode::Leaf { plan, def, columns } => match plan.as_ref() {
                Statement::Select(sel) if self.plain_leaf(sel) => {
                    Ok(Some(Prepared::Leaf { sel, def, columns }))
                }
                _ => Ok(None),
            },
            JoinNode::Lateral { .. } => Ok(None),
            JoinNode::Join { left, right, .. } => {
                let JoinNode::Leaf { plan, def, columns } = right.as_ref() else {
                    return Ok(None);
                };
                let Statement::Select(rsel) = plan.as_ref() else {
                    return Ok(None);
                };
                if !self.plain_leaf(rsel) {
                    return Ok(None);
                }
                let Some(left) = self.prepare_join(left, budget)? else {
                    return Ok(None);
                };
                let mut blobs: Vec<Vec<u8>> = Vec::new();
                let mut over = false;
                self.table_batches(&rsel.table, &rsel.filter, &mut |batch| {
                    let bytes: usize = batch.iter().map(Vec::len).sum();
                    if bytes > *budget {
                        over = true;
                        return Ok(false);
                    }
                    *budget -= bytes;
                    blobs.extend(batch);
                    Ok(true)
                })?;
                if over {
                    return Ok(None);
                }
                let right = self.leaf_docs(rsel, def, columns, &blobs)?;
                Ok(Some(Prepared::Join {
                    node,
                    left: Box::new(left),
                    right,
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

    /// A narrow join (`JoinSelect`) of two stored tables, its right side
    /// read and held, or `None`.
    fn prepared_narrow(
        &self,
        join: &secantus_pgplan::JoinSelect,
    ) -> PgWireResult<Option<Vec<Document>>> {
        if join.left_sub.is_some()
            || join.right_sub.is_some()
            || join.order.is_some()
            || !self.join_may_stream()
            || self.stream_table(&join.left.0).is_none()
            || self.stream_table(&join.right.0).is_none()
        {
            return Ok(None);
        }
        let mut budget = join_inner_bytes();
        let mut blobs: Vec<Vec<u8>> = Vec::new();
        let mut over = false;
        self.table_batches(&join.right.0, &Document::new(), &mut |batch| {
            let bytes: usize = batch.iter().map(Vec::len).sum();
            if bytes > budget {
                over = true;
                return Ok(false);
            }
            budget -= bytes;
            blobs.extend(batch);
            Ok(true)
        })?;
        if over {
            return Ok(None);
        }
        let rows = blobs
            .iter()
            .map(|b| decode_doc(b))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| Self::storage_err("could not decode a row", e))?;
        Ok(Some(rows))
    }

    /// Hand a narrow join's rows to `sink` a batch at a time.
    fn feed_narrow(
        &self,
        join: &secantus_pgplan::JoinSelect,
        right: Vec<Document>,
        sink: Sink<'_>,
    ) -> PgWireResult<()> {
        self.join_docs_core(
            join,
            &mut |inner_sink| {
                self.table_batches(&join.left.0, &Document::new(), &mut |blobs| {
                    let docs = blobs
                        .iter()
                        .map(|b| decode_doc(b))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| Self::storage_err("could not decode a row", e))?;
                    inner_sink(docs)
                })
            },
            right,
            sink,
        )
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
            Narrow(&'a secantus_pgplan::JoinSelect, Vec<Document>),
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

    /// Run a [`JoinFeed`], handing its rows on as stored-row blobs.
    pub(crate) fn run_join_feed(
        &self,
        feed: &mut JoinFeed<'_>,
        sink: &mut dyn FnMut(Vec<Vec<u8>>) -> bool,
    ) -> PgWireResult<()> {
        let Some(prepared) = feed.prepared.take() else {
            return Ok(());
        };
        let filter = feed.filter;
        let empty = Document::new();
        let in_sets = secantus_core::query::InSets::prepare(filter);
        let _in_sets = secantus_core::query::InSetsGuard::install(&in_sets);
        self.feed_join(prepared, &mut |docs| {
            let mut blobs = Vec::with_capacity(docs.len());
            for d in docs {
                if !filter.is_empty()
                    && !secantus_core::query::matches(&d, filter, &empty, None).unwrap_or(false)
                {
                    continue;
                }
                blobs.push(
                    bson::to_vec(&d)
                        .map_err(|e| Self::user_error("XX000", format!("a joined row: {e}")))?,
                );
            }
            Ok(sink(blobs))
        })
    }
}

/// A join prepared as an aggregate's input (see `join_aggregate_source`).
pub(crate) struct JoinFeed<'a> {
    prepared: Option<Prepared<'a>>,
    filter: &'a Document,
}
