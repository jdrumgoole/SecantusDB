//! Streaming a read-only extended-protocol portal outside a transaction
//! block.
//!
//! An Execute used to materialise its whole result before the first row
//! went out, so a large SELECT held every row in memory however the client
//! fetched it. A WiredTiger session is thread-affine and an Execute runs on
//! whichever tokio worker polls the connection, so a storage cursor cannot
//! simply be left open between two Executes. Instead the portal gets a
//! THREAD of its own, which owns a WiredTiger session, its snapshot and its
//! cursor (`Storage::scan_matching_batches`), and hands the rows over in
//! batches through a bounded channel. pgwire's portal pulls `max_rows` at a
//! time and answers PortalSuspended; between Executes the thread is parked
//! on the full channel, so the server holds at most a few batches of the
//! result. Dropping the portal (Close, the end of the group at Sync, the
//! connection) closes the channel and the thread ends with its session.
//!
//! Only where the read needs nothing the block owns:
//! - an extended-protocol statement outside a transaction block, whose
//!   implicit group has written nothing, at READ COMMITTED (the portal's own
//!   snapshot is then a statement snapshot, as PostgreSQL's is). Inside a
//!   block the portal must read the BLOCK's snapshot, which lives on the
//!   block's session, so a block's portal is materialised as before;
//! - a plain SELECT of stored columns from one stored table of built-in
//!   types: no join, subquery source, series, window, DISTINCT, per-row
//!   WHERE residual or computed column (each of those needs every row, or
//!   thread-local session state, before the first row can go out). An ORDER
//!   BY of stored columns is sorted in bounded memory (`external_sort`): by
//!   the reader outside a block, and inside one (a portal or a `DECLARE
//!   CURSOR`) at the first fetch, through the block's transaction;
//! - with a WHERE, when the client fetches in pieces (`max_rows > 0`), or
//!   when the storage would answer the WHERE by a collection scan anyway:
//!   the streamed read is a collection scan, which an indexed lookup should
//!   not be traded for when the whole result is wanted at once.
//!
//! A cancel and `statement_timeout` are checked before each batch: the
//! thread sends the error in place of the next rows, and the Execute that
//! reaches it fails with 57014 as the statement would have. The timeout
//! counts the reader's working time only (PostgreSQL times each Execute, so
//! a client's pause between two fetches is not the statement running). A
//! cancel that arrives while the client is idle between two fetches is seen
//! at the next one -- PostgreSQL would ignore it, there being no statement
//! running at that moment.

use super::*;

/// Rows per hand-off, and how many hand-offs may wait in the channel.
const BATCH: usize = 256;
const IN_FLIGHT: usize = 2;

/// May the next statement be streamed: never, only without a WHERE, or
/// whatever its WHERE (see the module doc).
pub(crate) const STREAM_NEVER: u8 = 0;
pub(crate) const STREAM_UNFILTERED: u8 = 1;
pub(crate) const STREAM_ANY: u8 = 2;

type Job = Box<dyn FnOnce() + Send>;

/// Idle reader threads, each waiting on its own job channel: a portal reuses
/// one rather than paying a thread start for every streamed SELECT (a small
/// result's whole cost is otherwise mostly the spawn).
fn idle_readers() -> &'static std::sync::Mutex<Vec<std::sync::mpsc::Sender<Job>>> {
    static IDLE: std::sync::OnceLock<std::sync::Mutex<Vec<std::sync::mpsc::Sender<Job>>>> =
        std::sync::OnceLock::new();
    IDLE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// At most this many reader threads wait idle; more are let go.
const MAX_IDLE_READERS: usize = 16;

/// Run `job` on a reader thread: an idle one, or a new one that parks itself
/// when the job is done.
fn run_on_reader(job: Job) -> std::io::Result<()> {
    let mut job = Some(job);
    loop {
        let idle = idle_readers()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop();
        let Some(worker) = idle else { break };
        match worker.send(job.take().expect("unsent")) {
            Ok(()) => return Ok(()),
            // That worker is gone: take the job back and try another.
            Err(std::sync::mpsc::SendError(back)) => job = Some(back),
        }
    }
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    tx.send(job.take().expect("unsent"))
        .map_err(|_| std::io::Error::other("reader channel closed"))?;
    std::thread::Builder::new()
        .name("secantusd-pg-portal".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                job();
                let mut idle = idle_readers().lock().unwrap_or_else(|e| e.into_inner());
                if idle.len() >= MAX_IDLE_READERS {
                    return;
                }
                idle.push(tx.clone());
            }
        })?;
    Ok(())
}

impl PgHandler {
    /// Set by an extended Execute before it runs its statement: may the
    /// SELECT it plans be streamed? (`STREAM_*`; armed after planning, for a
    /// top-level SELECT only.)
    pub(crate) fn allow_portal_stream(&self, max_rows: usize) {
        let in_block = self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed);
        let group = self
            .implicit_extended
            .load(std::sync::atomic::Ordering::Relaxed);
        let clean = self
            .txn
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_none_or(|h| !h.has_written());
        let failed = self.txn_failed.load(std::sync::atomic::Ordering::Relaxed);
        self.stream_in_block
            .store(in_block, std::sync::atomic::Ordering::Relaxed);
        let mode = if in_block && !failed {
            // Inside a block the portal reads through the block's own
            // transaction, one batch per fetch (`BlockScan`).
            if max_rows > 0 {
                STREAM_ANY
            } else {
                STREAM_UNFILTERED
            }
        } else if !in_block && group && clean && self.read_committed_default() {
            if max_rows > 0 {
                STREAM_ANY
            } else {
                STREAM_UNFILTERED
            }
        } else {
            STREAM_NEVER
        };
        self.stream_request
            .store(mode, std::sync::atomic::Ordering::Relaxed);
    }

    /// Set by a simple Query of ONE statement that runs no user code, before
    /// it runs: outside any transaction (no block, no extended group open)
    /// at READ COMMITTED, the SELECT it plans may stream on a reader thread
    /// of its own snapshot -- the statement's snapshot, as PostgreSQL's --
    /// and pgwire sends its rows as they arrive instead of the whole result
    /// being built first. Without a WHERE, or with one the storage would
    /// answer by a collection scan (`STREAM_UNFILTERED`): the whole result
    /// is wanted at once, so an indexed lookup is not traded for the
    /// reader's collection scan.
    pub(crate) fn allow_simple_stream(&self) {
        let idle = self.txn.lock().unwrap_or_else(|e| e.into_inner()).is_none();
        let in_block = self
            .in_transaction
            .load(std::sync::atomic::Ordering::Relaxed);
        let failed = self.txn_failed.load(std::sync::atomic::Ordering::Relaxed);
        // Batch 64: inside a block the SELECT streams through the block's
        // own transaction, a batch each time pgwire polls the response
        // (`BlockScan`, as an extended portal in a block) -- the response is
        // sent in full before the next message is read, and any statement
        // first drains a scan still open (`drain_block_scans`). Batch 65:
        // under READ COMMITTED too. The statement's snapshot is then the
        // block's own -- refreshed for it, or the block moved onto a fresh
        // one (`with_isolation_judged`) -- which the scan reads through;
        // a statement that instead runs APART from the block, in a fresh
        // transaction that ends when it returns (`read_apart`), keeps the
        // materialised path (`running_apart`, checked in
        // `try_stream_select`): a scan polled later would read the block's
        // older snapshot, which is what the first version of batch 64 did.
        if in_block && !idle && !failed {
            self.stream_in_block
                .store(true, std::sync::atomic::Ordering::Relaxed);
            self.stream_request
                .store(STREAM_UNFILTERED, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let mode = if idle
            && !self
                .in_transaction
                .load(std::sync::atomic::Ordering::Relaxed)
            && !self
                .implicit_extended
                .load(std::sync::atomic::Ordering::Relaxed)
            && !self.txn_failed.load(std::sync::atomic::Ordering::Relaxed)
            && self.read_committed_default()
        {
            STREAM_UNFILTERED
        } else {
            STREAM_NEVER
        };
        self.stream_in_block
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.stream_request
            .store(mode, std::sync::atomic::Ordering::Relaxed);
    }

    /// The SELECT as a streamed result, or `None` to run it as before.
    pub(crate) fn try_stream_select(
        &self,
        sel: &secantus_pgplan::Select,
        env: &RowEnv,
    ) -> PgWireResult<Option<Response>> {
        let mode = self
            .stream_portal
            .swap(STREAM_NEVER, std::sync::atomic::Ordering::Relaxed);
        // A WHERE the storage would answer by an index keeps that route when
        // the whole result is wanted at once; one it would answer by a
        // collection scan anyway streams (batch 63) -- the reader's scan is
        // the same read, in bounded memory.
        // Batch 64: an indexed WHERE streams too -- the reader walks the
        // same index route a batch at a time (`scan_routed_batches`), so no
        // cardinality estimate is needed. A primary-key point lookup alone
        // keeps the direct path: one row, and a reader thread would only
        // add its hand-off.
        // Inside a block the scan reads through the block's transaction
        // by collection scan (`BlockScan`), so there an indexed WHERE keeps
        // the index route when the whole result is wanted at once.
        let block_indexed = mode == STREAM_UNFILTERED
            && self
                .stream_in_block
                .load(std::sync::atomic::Ordering::Relaxed)
            && !sel.filter.is_empty()
            && !matches!(
                self.storage
                    .explain_plan(self.db(), &sel.table, &sel.filter),
                Ok(secantus_storage::ExplainPlan::CollScan)
            );
        // A READ COMMITTED statement running apart from its block reads in
        // a transaction that ends when it returns: materialised.
        if self
            .stream_in_block
            .load(std::sync::atomic::Ordering::Relaxed)
            && self
                .running_apart
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            self.stream_in_block
                .store(false, std::sync::atomic::Ordering::Relaxed);
            return Ok(None);
        }
        // A join (batch 65): read now, on this thread, in bounded memory
        // (`stream_join`).
        if mode != STREAM_NEVER && (sel.sub.is_some() || sel.join.is_some()) {
            self.stream_in_block
                .store(false, std::sync::atomic::Ordering::Relaxed);
            return self.try_stream_join_select(sel, env);
        }
        if mode == STREAM_NEVER
            || block_indexed
            || (mode == STREAM_UNFILTERED && self.primary_key_point(sel))
        {
            return Ok(None);
        }
        // An ORDER BY of stored columns is sorted in bounded memory
        // (`external_sort`): outside a block by the reader thread, inside
        // one at the portal's first fetch through the block's transaction.
        let Some(def) = self.streamable(sel, true) else {
            return Ok(None);
        };
        // A `DECLARE CURSOR` capture needs the rows as they are encoded on
        // this thread.
        if self
            .cursor_capture
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            return Ok(None);
        }
        self.stream_select_of(sel, &def, env)
    }

    /// Is `sel`'s WHERE an equality (or `IN` list) on the `_id` field alone,
    /// which the storage answers by a point lookup?
    fn primary_key_point(&self, sel: &secantus_pgplan::Select) -> bool {
        if sel.filter.len() != 1 {
            return false;
        }
        match sel.filter.get("_id") {
            Some(Bson::Document(d)) => {
                d.len() == 1 && (d.contains_key("$eq") || d.contains_key("$in"))
            }
            Some(_) => true,
            None => false,
        }
    }

    /// The table definition of `sel` when it is a plain read of one stored
    /// table that can be streamed (see the module doc), else `None`.
    fn streamable(&self, sel: &secantus_pgplan::Select, sorted: bool) -> Option<TableDef> {
        // A `FOR UPDATE` locks the rows it returns, which the reader
        // thread's own session could not do for this transaction.
        let plain = sel.lock.is_none()
            && sel.lock_clauses.is_empty()
            && sel.sub.is_none()
            && sel.join.is_none()
            && sel.series.is_none()
            && sel.windows.is_empty()
            && (sel.order.is_empty() || sorted)
            && (sel.distinct == secantus_pgplan::Distinct::None || sorted)
            && sel.residual.is_none()
            && sel.casts.iter().all(Option::is_none)
            && sel.offset >= 0
            && !sel.table.is_empty()
            && Self::virtual_table(&sel.table).is_none();
        if !plain {
            return None;
        }
        let def = self.lookup(&sel.table)?;
        // A sequence read as a relation, and anything of a type this
        // session resolves from its own catalog, keep the materialised path.
        let builtin = def
            .columns
            .iter()
            .all(|c| secantus_pgplan::pgtypes::oid_of_name(&c.pg_type).is_some());
        if !builtin || def.column("is_called").is_some() || def.temp {
            return None;
        }
        self.stream_order(sel, &def)?;
        Some(def)
    }

    /// The order a streamed `sel` is sorted in, and the fields its DISTINCT
    /// (or DISTINCT ON) de-duplicates on, or `None` when it cannot stream.
    ///
    /// A DISTINCT is sorted on its ORDER BY and then on every output field,
    /// so equal rows meet and the first of each run is kept
    /// (`external_sort::sort_runs_with`): the sort-based DISTINCT
    /// PostgreSQL's planner also uses, in bounded memory. Without an ORDER
    /// BY its rows come out in that field order, which PostgreSQL leaves
    /// unspecified too. A DISTINCT ON's keys lead its ORDER BY (PostgreSQL
    /// requires it), or are the order when there is none.
    pub(crate) fn stream_order(
        &self,
        sel: &secantus_pgplan::Select,
        def: &TableDef,
    ) -> Option<(Vec<OrderKey>, Option<Vec<String>>)> {
        let (order, mut dedup) = self.stream_order_of(sel, def)?;
        // A timestamp is its millisecond date plus the hidden
        // sub-millisecond companion (present only when non-zero, as every
        // write path keeps it), so the companion is part of a DISTINCT's
        // identity; the sort already orders equal dates by it
        // (`compare_rows`), so equal timestamps meet.
        let stamp = |f: &str| {
            def.columns.iter().any(|c| {
                c.field() == f
                    && matches!(
                        secantus_pgplan::pgtypes::oid_of_name(&c.pg_type),
                        Some(1114 | 1184)
                    )
            })
        };
        if let Some(fields) = dedup.as_mut() {
            let extra: Vec<String> = fields
                .iter()
                .filter(|f| stamp(f))
                .map(|f| secantus_pgplan::companion_field(f))
                .filter(|c| !fields.contains(c))
                .collect();
            fields.extend(extra);
        }
        Some((order, dedup))
    }

    fn stream_order_of(
        &self,
        sel: &secantus_pgplan::Select,
        def: &TableDef,
    ) -> Option<(Vec<OrderKey>, Option<Vec<String>>)> {
        let asc = |f: &str| OrderKey {
            field: f.to_string(),
            ascending: true,
            nulls: secantus_pgplan::Nulls::Last,
            expr: None,
        };
        match &sel.distinct {
            secantus_pgplan::Distinct::None => Some((sel.order.clone(), None)),
            secantus_pgplan::Distinct::All => {
                // The identity is the stored value, except for a tsvector /
                // tsquery, whose output the materialised path reassembles
                // from a document when the Python server wrote it: there the
                // identity is its TEXT, computed per row into a hidden field
                // (`fts_text_key`) that the sort and the de-duplication use
                // in the column's place (batch 64).
                let schema = self.row_schema(def, &sel.columns, &sel.casts);
                let mut fields: Vec<String> = Vec::new();
                let mut order = sel.order.clone();
                for (i, (_, f)) in sel.columns.iter().enumerate() {
                    let fts = schema.get(i).is_some_and(|c| {
                        let t = c.datatype();
                        *t == Type::TS_VECTOR || *t == Type::TSQUERY
                    });
                    if fts {
                        // An ORDER BY of the column itself sorts by the
                        // stored value, which would keep equal texts of two
                        // spellings apart: materialised, as before.
                        if sel.order.iter().any(|k| k.expr.is_none() && k.field == *f) {
                            return None;
                        }
                        let key = fts_text_key(i, f);
                        fields.push(key.field.clone());
                        if !order.iter().any(|k| k.field == key.field) {
                            order.push(key);
                        }
                        continue;
                    }
                    fields.push(f.clone());
                    if !order.iter().any(|k| k.expr.is_none() && k.field == *f) {
                        order.push(asc(f));
                    }
                }
                Some((order, Some(fields)))
            }
            secantus_pgplan::Distinct::On(keys) => {
                let stored = |k: &String| def.columns.iter().any(|c| c.field() == *k);
                if !keys.iter().all(stored) {
                    return None;
                }
                if sel.order.is_empty() {
                    return Some((keys.iter().map(|k| asc(k)).collect(), Some(keys.clone())));
                }
                let lead = sel.order.get(..keys.len())?;
                let leads = lead
                    .iter()
                    .all(|k| k.expr.is_none() && keys.contains(&k.field))
                    && keys.iter().all(|k| lead.iter().any(|o| o.field == *k));
                leads.then(|| (sel.order.clone(), Some(keys.clone())))
            }
        }
    }

    /// `sel` (see `streamable`) as a streamed result.
    fn stream_select_of(
        &self,
        sel: &secantus_pgplan::Select,
        def: &TableDef,
        env: &RowEnv,
    ) -> PgWireResult<Option<Response>> {
        let schema = Arc::new(self.row_schema(def, &sel.columns, &sel.casts));
        let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
        let in_block = self
            .stream_in_block
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        let Some((order, dedup)) = self.stream_order(sel, def) else {
            return Ok(None);
        };
        let source: futures::stream::BoxStream<'static, PgWireResult<Vec<Document>>> = if in_block {
            self.block_scan_stream(sel, order, dedup)?
        } else if order.iter().any(|k| k.expr.is_some()) {
            // An ORDER BY expression is evaluated with this session's state,
            // which only this thread has installed: the rows are read and
            // sorted here, now, in bounded memory, and handed out of the
            // merge a batch per fetch.
            self.sorted_here(sel, order, dedup)?
        } else {
            let rx = self.spawn_portal_reader(sel, order, dedup)?;
            futures::stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|item| (item, rx))
            })
            .boxed()
        };
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

    /// `sel`'s rows read and sorted on this thread (outside a block), then
    /// handed out of the merge a batch at a time.
    fn sorted_here(
        &self,
        sel: &secantus_pgplan::Select,
        order: Vec<OrderKey>,
        dedup: Option<Vec<String>>,
    ) -> PgWireResult<futures::stream::BoxStream<'static, PgWireResult<Vec<Document>>>> {
        let skip = usize::try_from(sel.offset).unwrap_or(0);
        let left = sel
            .limit
            .map(|l| usize::try_from(l.max(0)).unwrap_or(usize::MAX));
        let mut stopped: Option<PgWireError> = None;
        let mut failed: Option<PgWireError> = None;
        let keys = order.clone();
        let sorted = crate::external_sort::sort_runs_with(
            |sink| {
                self.storage
                    .scan_routed_batches(self.db(), &sel.table, &sel.filter, BATCH, |blobs| {
                        if let Err(e) = self.check_cancel() {
                            stopped = Some(e);
                            return false;
                        }
                        sink(blobs)
                    })
                    .map_err(|e| e.to_string())
            },
            &order,
            skip,
            left,
            BATCH,
            dedup,
            &mut |d| compute_order_keys(&keys, d, &mut failed),
        );
        if let Some(e) = stopped.or(failed) {
            return Err(e);
        }
        let sorted =
            sorted.map_err(|e| PgHandler::user_error("XX000", format!("could not sort: {e}")))?;
        Ok(self.sorted_stream(sorted))
    }

    /// Rows already sorted (or gathered) on this thread, handed out of the
    /// merge a batch per poll.
    pub(crate) fn sorted_stream(
        &self,
        sorted: crate::external_sort::Sorted,
    ) -> futures::stream::BoxStream<'static, PgWireResult<Vec<Document>>> {
        let backend = self.backend.clone();
        futures::stream::unfold(Some(sorted), move |state| {
            let backend = backend.clone();
            async move {
                let mut sorted = state?;
                let out = crate::blocking_wait(|| -> PgWireResult<Vec<Document>> {
                    if backend.terminate.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(PgHandler::admin_shutdown());
                    }
                    if backend.cancelled() {
                        return Err(PgHandler::query_canceled());
                    }
                    sorted
                        .next_batch()
                        .map_err(|e| PgHandler::user_error("XX000", e))
                });
                match out {
                    Ok(docs) if docs.is_empty() => None,
                    Ok(docs) => Some((Ok(docs), Some(sorted))),
                    Err(e) => Some((Err(e), None)),
                }
            }
        })
        .boxed()
    }

    /// Start the thread that reads `sel`'s rows and hands them over.
    fn spawn_portal_reader(
        &self,
        sel: &secantus_pgplan::Select,
        order: Vec<OrderKey>,
        dedup: Option<Vec<String>>,
    ) -> PgWireResult<tokio::sync::mpsc::Receiver<PgWireResult<Vec<Document>>>> {
        let (tx, rx) = tokio::sync::mpsc::channel::<PgWireResult<Vec<Document>>>(IN_FLIGHT);
        let storage = self.storage.clone();
        let db = self.db().to_string();
        let table = sel.table.clone();
        let filter = sel.filter.clone();
        let mut skip = usize::try_from(sel.offset).unwrap_or(0);
        let mut left = sel
            .limit
            .map(|l| usize::try_from(l.max(0)).unwrap_or(usize::MAX));
        let backend = self.backend.clone();
        // `statement_timeout` counts the time the statement WORKS, not the
        // time the client takes between two Executes of a suspended portal
        // (PostgreSQL times each Execute): the reader's budget is what is left
        // of the statement's, spent only while it reads.
        let budget = self
            .backend
            .deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|d| d.saturating_duration_since(std::time::Instant::now()));
        run_on_reader(Box::new(move || {
            if left == Some(0) {
                return;
            }
            let mut worked = std::time::Duration::ZERO;
            let mut since = std::time::Instant::now();
            let stop = |tx: &tokio::sync::mpsc::Sender<PgWireResult<Vec<Document>>>,
                        worked: std::time::Duration| {
                let err = if backend.terminate.load(std::sync::atomic::Ordering::Relaxed) {
                    Some(PgHandler::admin_shutdown())
                } else if backend.cancelled() {
                    Some(PgHandler::query_canceled())
                } else if budget.is_some_and(|b| worked >= b) {
                    Some(PgHandler::user_error(
                        "57014",
                        "canceling statement due to statement timeout".into(),
                    ))
                } else {
                    None
                };
                // The receiver may already be gone (the portal was
                // closed): then nobody is waiting for the error either.
                err.map(|e| tx.blocking_send(Err(e)).is_ok())
            };
            if !order.is_empty() {
                // An ORDER BY: every row is read before the first goes out,
                // sorted in bounded memory (`external_sort`).
                let stopped = std::cell::Cell::new(false);
                let worked = std::cell::Cell::new(worked);
                let since = std::cell::Cell::new(since);
                // Work since the last hand-off; `false` once stopped.
                let tick = || {
                    worked.set(worked.get() + since.get().elapsed());
                    since.set(std::time::Instant::now());
                    if stop(&tx, worked.get()).is_some() {
                        stopped.set(true);
                    }
                    !stopped.get()
                };
                let sorted = crate::external_sort::sort_runs_with(
                    |sink| {
                        storage
                            .scan_routed_batches(&db, &table, &filter, BATCH, |blobs| {
                                tick() && sink(blobs)
                            })
                            .map_err(|e| e.to_string())
                    },
                    &order,
                    skip,
                    left,
                    BATCH,
                    dedup,
                    &mut |_| Ok(()),
                )
                .and_then(|mut sorted| loop {
                    let docs = sorted.next_batch()?;
                    if docs.is_empty() || !tick() {
                        return Ok(());
                    }
                    if tx.blocking_send(Ok(docs)).is_err() {
                        return Ok(());
                    }
                    // Waiting for the client to fetch is not work.
                    since.set(std::time::Instant::now());
                });
                if let Err(e) = sorted {
                    if !stopped.get() {
                        let _sent = tx.blocking_send(Err(PgHandler::user_error("XX000", e)));
                    }
                }
                return;
            }
            let scanned = storage.scan_routed_batches(&db, &table, &filter, BATCH, |blobs| {
                worked += since.elapsed();
                since = std::time::Instant::now();
                if stop(&tx, worked).is_some() {
                    return false;
                }
                let mut docs = Vec::with_capacity(blobs.len());
                for blob in blobs {
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    match decode_doc(&blob) {
                        Ok(d) => docs.push(d),
                        Err(e) => {
                            let _sent = tx.blocking_send(Err(PgHandler::storage_err(
                                "could not decode a row",
                                e,
                            )));
                            return false;
                        }
                    }
                    if let Some(n) = left.as_mut() {
                        *n -= 1;
                        if *n == 0 {
                            break;
                        }
                    }
                }
                worked += since.elapsed();
                if !docs.is_empty() && tx.blocking_send(Ok(docs)).is_err() {
                    // The portal was dropped: stop reading.
                    return false;
                }
                // Waiting for the client to fetch is not work.
                since = std::time::Instant::now();
                left != Some(0)
            });
            if let Err(e) = scanned {
                let _sent = tx.blocking_send(Err(PgHandler::storage_err("could not read", e)));
            }
        }))
        .map_err(|e| PgHandler::storage_err("could not start a portal reader", e))?;
        Ok(rx)
    }
}

/// A portal of a transaction BLOCK streamed through the block's own
/// transaction: the rows it has not yet handed out are read one batch per
/// fetch, on the block's session, so they see the block's snapshot and its
/// own uncommitted writes. A WiredTiger session may be used by one thread
/// at a time, not only by one thread, so each batch is read on whichever
/// thread polls the portal, holding the block's transaction lock.
///
/// The portal's snapshot is the one its statement ran in. Any other
/// statement of the block would refresh that snapshot (READ COMMITTED) or
/// write, so before one runs, every open block scan reads its rest into
/// memory (`drain_block_scans`): the result is then exactly what it would
/// have been, and memory is bounded whenever the client fetches the portal
/// without interleaving other statements -- the cursor-fetch pattern
/// (pgjdbc's `setFetchSize`) this exists for.
#[derive(Clone)]
pub(crate) struct BlockScan {
    db: String,
    table: String,
    filter: Document,
    /// Resume after this RecordId (`None`: from the start).
    after: Option<i64>,
    /// The collection is exhausted (or OFFSET / LIMIT is satisfied).
    done: bool,
    /// Rows read by a drain, handed out before anything else.
    pending: std::collections::VecDeque<Document>,
    skip: usize,
    left: Option<usize>,
    /// The ORDER BY (stored columns only); empty for scan order.
    order: Vec<OrderKey>,
    /// With an ORDER BY: the rows sorted in bounded memory
    /// (`external_sort`), read whole at the first batch.
    sorted: Option<Arc<Mutex<crate::external_sort::Sorted>>>,
    /// A DISTINCT's fields (`stream_order`).
    dedup: Option<Vec<String>>,
}

/// Compute each ORDER BY expression into its synthetic field, as the
/// materialised path does before it sorts. The first error is kept in
/// `failed` with its SQLSTATE; the sort sees only that it stopped.
/// The hidden sort / DISTINCT key holding a tsvector / tsquery column's
/// TEXT (`compute_order_keys`): a value the Python server wrote is a
/// document, one this server wrote is that text already. The marker is a
/// `Casts` whose `source` names the stored field and whose chain is
/// [`FTS_TEXT`].
fn fts_text_key(i: usize, field: &str) -> OrderKey {
    OrderKey {
        field: format!("__dfts{i}"),
        ascending: true,
        nulls: secantus_pgplan::Nulls::Last,
        expr: Some(secantus_pgplan::ColumnExpr::Casts {
            source: Some(field.to_string()),
            chain: vec![FTS_TEXT.to_string()],
        }),
    }
}

/// See [`fts_text_key`].
const FTS_TEXT: &str = "\u{0}fts_text";

pub(crate) fn compute_order_keys(
    order: &[OrderKey],
    d: &mut Document,
    failed: &mut Option<PgWireError>,
) -> Result<(), String> {
    for key in order {
        let Some(expr) = key.expr.as_ref() else {
            continue;
        };
        if let secantus_pgplan::ColumnExpr::Casts {
            source: Some(field),
            chain,
        } = expr
        {
            if chain.len() == 1 && chain[0] == FTS_TEXT {
                let v = match d.get(field) {
                    Some(v) => secantus_pgplan::fts::python_text(v)
                        .map(Bson::String)
                        .unwrap_or_else(|| v.clone()),
                    None => Bson::Null,
                };
                d.insert(key.field.clone(), v);
                continue;
            }
        }
        match secantus_pgplan::apply_row_expr(expr, d) {
            Ok(v) => {
                d.insert(key.field.clone(), v);
            }
            Err(e) => {
                *failed = Some(PgHandler::err(&e));
                return Err("an ORDER BY expression failed".into());
            }
        }
    }
    Ok(())
}

impl BlockScan {
    /// Read the next batch on the CURRENT session (inside the block's
    /// transaction), applying OFFSET and LIMIT. `check` is called between
    /// batches read from storage (a cancel, a terminate).
    fn read_batch(
        &mut self,
        storage: &Storage,
        check: &dyn Fn() -> PgWireResult<()>,
    ) -> PgWireResult<Vec<Document>> {
        if !self.order.is_empty() {
            return self.read_sorted_batch(storage, check);
        }
        let mut docs = Vec::new();
        while docs.is_empty() && !self.done {
            let (blobs, next) = storage
                .scan_batch_after(&self.db, &self.table, &self.filter, self.after, BATCH)
                .map_err(|e| PgHandler::storage_err("could not read", e))?;
            self.after = next;
            if next.is_none() {
                self.done = true;
            }
            for blob in blobs {
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                docs.push(
                    decode_doc(&blob)
                        .map_err(|e| PgHandler::storage_err("could not decode a row", e))?,
                );
                if let Some(n) = self.left.as_mut() {
                    *n -= 1;
                    if *n == 0 {
                        self.done = true;
                        break;
                    }
                }
            }
        }
        Ok(docs)
    }

    /// An ordered scan's next batch: the first reads every row of the
    /// snapshot into sorted runs (spilled past `external_sort`'s run size,
    /// so memory stays bounded), the rest are handed out of their merge.
    fn read_sorted_batch(
        &mut self,
        storage: &Storage,
        check: &dyn Fn() -> PgWireResult<()>,
    ) -> PgWireResult<Vec<Document>> {
        if self.done {
            return Ok(Vec::new());
        }
        self.prime(storage, check)?;
        let docs = self
            .sorted
            .as_ref()
            .expect("set by prime")
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_batch()
            .map_err(|e| PgHandler::user_error("XX000", e))?;
        if docs.is_empty() {
            self.done = true;
            self.sorted = None;
        }
        Ok(docs)
    }

    /// Read every row into the sorted runs, once (see `read_sorted_batch`).
    fn prime(
        &mut self,
        storage: &Storage,
        check: &dyn Fn() -> PgWireResult<()>,
    ) -> PgWireResult<()> {
        if self.sorted.is_none() && !self.done {
            let mut stopped: Option<PgWireError> = None;
            let mut failed: Option<PgWireError> = None;
            let order = self.order.clone();
            let sorted = crate::external_sort::sort_runs_with(
                |sink| {
                    let mut after = None;
                    loop {
                        if let Err(e) = check() {
                            stopped = Some(e);
                            return Ok(());
                        }
                        let (blobs, next) = storage
                            .scan_batch_after(&self.db, &self.table, &self.filter, after, BATCH)
                            .map_err(|e| e.to_string())?;
                        if !sink(blobs) || next.is_none() {
                            return Ok(());
                        }
                        after = next;
                    }
                },
                &self.order,
                self.skip,
                self.left,
                BATCH,
                self.dedup.clone(),
                &mut |d| compute_order_keys(&order, d, &mut failed),
            );
            if let Some(e) = stopped.or(failed) {
                return Err(e);
            }
            let sorted = sorted
                .map_err(|e| PgHandler::user_error("XX000", format!("could not sort: {e}")))?;
            self.sorted = Some(Arc::new(Mutex::new(sorted)));
        }
        Ok(())
    }
}

impl BlockScan {
    /// A scan of `sel`'s rows from the start, in `order` (`stream_order`).
    fn of(
        db: &str,
        sel: &secantus_pgplan::Select,
        order: Vec<OrderKey>,
        dedup: Option<Vec<String>>,
    ) -> Self {
        BlockScan {
            db: db.to_string(),
            table: sel.table.clone(),
            filter: sel.filter.clone(),
            after: None,
            done: sel.limit == Some(0),
            pending: std::collections::VecDeque::new(),
            skip: usize::try_from(sel.offset).unwrap_or(0),
            left: sel
                .limit
                .map(|l| usize::try_from(l.max(0)).unwrap_or(usize::MAX)),
            order,
            sorted: None,
            dedup,
        }
    }
}

/// Rows a streamed cursor keeps behind its position, so a short step back
/// needs no re-read.
const KEEP_BEHIND: usize = BATCH;

/// A `DECLARE CURSOR` over a plain read of one table, read a batch per
/// FETCH through the block's transaction rather than all at DECLARE. Its
/// `CursorState` holds only a window of the rows (from `base + 1`); a step
/// back past the window reads the rows again from the start, which gives
/// the same rows because the snapshot is the DECLARE's: a FETCH / MOVE /
/// CLOSE keeps it, and any other statement first reads the cursor whole
/// (`drain_block_scans`), which is the materialised cursor the rest of the
/// server knows. So is a `WITH HOLD` cursor at COMMIT, as PostgreSQL
/// materialises one there.
pub(crate) struct CursorTail {
    /// Every row has been read (the window may still have dropped some, so
    /// the tail stays for a re-read from the start).
    exhausted: bool,
    scan: BlockScan,
    start: BlockScan,
    schema: Arc<Vec<FieldInfo>>,
    fields: Vec<String>,
    casts: Vec<Option<secantus_pgplan::ColumnExpr>>,
    env: RowEnv,
}

/// Drop the rows of a streamed cursor more than `KEEP_BEHIND` behind its
/// position.
pub(crate) fn trim_cursor(cursor: &mut CursorState) {
    let behind = usize::try_from(cursor.pos.max(1) - 1).unwrap_or(0);
    let drop_to = behind.saturating_sub(KEEP_BEHIND);
    if drop_to > cursor.base {
        let n = (drop_to - cursor.base).min(cursor.rows.len());
        cursor.rows.drain(..n);
        if let Some(t) = cursor.typed_rows.as_mut() {
            t.drain(..n.min(t.len()));
        }
        cursor.base += n;
    }
}

impl PgHandler {
    /// A streamed cursor's state for `DECLARE ... FOR sel`, or `None` when
    /// `sel` is not a plain read of one table (it is then materialised).
    pub(crate) fn declare_streamed(
        &self,
        sel: &secantus_pgplan::Select,
        statement: &str,
        scrollable: bool,
        holdable: bool,
    ) -> PgWireResult<Option<CursorState>> {
        let Some(def) = self.streamable(sel, true) else {
            return Ok(None);
        };
        let tz = self.session_timezone();
        let env = RowEnv {
            tz: tz.clone(),
            ds: self.session_datestyle(),
            cenc: self.client_encoding(),
        };
        let Some((order, dedup)) = self.stream_order(sel, &def) else {
            return Ok(None);
        };
        let mut start = BlockScan::of(self.db(), sel, order, dedup);
        // An ORDER BY expression is evaluated with this session's state,
        // which only a statement's own thread has installed: sorted now.
        // (A re-read from the start happens in a FETCH or a statement too.)
        let mut first = start.clone();
        if start.order.iter().any(|k| k.expr.is_some()) {
            self.in_open_transaction(|| first.prime(&self.storage, &|| self.check_cancel()))?;
            start.sorted = None;
        }
        Ok(Some(CursorState {
            schema: Arc::new(self.row_schema(&def, &sel.columns, &sel.casts)),
            rows: Vec::new(),
            pos: 0,
            statement: statement.to_string(),
            is_holdable: holdable,
            is_binary: false,
            is_scrollable: scrollable,
            creation_time: bson::DateTime::now(),
            typed_rows: Some(Vec::new()),
            tz,
            tail: Some(CursorTail {
                exhausted: false,
                scan: first,
                start,
                schema: Arc::new(self.row_schema(&def, &sel.columns, &sel.casts)),
                fields: sel.columns.iter().map(|(_, f)| f.clone()).collect(),
                casts: sel.casts.clone(),
                env,
            }),
            base: 0,
        }))
    }

    /// Encode a streamed cursor's documents: the text rows and the values
    /// a binary FETCH re-encodes.
    fn encode_cursor_docs(
        &self,
        tail: &CursorTail,
        docs: Vec<Document>,
    ) -> PgWireResult<(Vec<DataRow>, CapturedRows)> {
        let cap: Arc<Mutex<Option<CapturedRows>>> = Arc::new(Mutex::new(Some(Vec::new())));
        let resp = self.project_stream_into(
            stream::iter(docs.into_iter().map(Ok)).boxed(),
            tail.schema.clone(),
            tail.fields.clone(),
            tail.casts.clone(),
            &tail.env,
            Some(cap.clone()),
        );
        let rows = futures::executor::block_on(resp.data_rows.try_collect::<Vec<_>>())?;
        let typed = cap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_default();
        Ok((rows, typed))
    }

    /// Read a streamed cursor's next batch into its window; `false` once
    /// it is exhausted (the tail is then dropped: the window holds the
    /// rest).
    fn pull_cursor(&self, cursor: &mut CursorState) -> PgWireResult<bool> {
        let Some(tail) = cursor.tail.as_mut() else {
            return Ok(false);
        };
        if tail.exhausted {
            return Ok(false);
        }
        let docs = tail
            .scan
            .read_batch(&self.storage, &|| self.check_cancel())?;
        tail.exhausted = tail.scan.done && tail.scan.pending.is_empty();
        let exhausted = tail.exhausted;
        if !docs.is_empty() {
            let tail = cursor.tail.as_ref().expect("checked");
            let (rows, typed) = self.encode_cursor_docs(tail, docs)?;
            cursor.rows.extend(rows);
            cursor.typed_rows.get_or_insert_with(Vec::new).extend(typed);
        }
        // Read whole with nothing dropped: an ordinary materialised cursor.
        if exhausted && cursor.base == 0 {
            cursor.tail = None;
        }
        Ok(!exhausted)
    }

    /// Read what a FETCH / MOVE of `direction` `count` reaches into a
    /// streamed cursor's window: up to the furthest row it moves to, from
    /// the start again when it moves back past the window, and to the end
    /// when it counts from the end.
    pub(crate) fn fill_cursor(
        &self,
        cursor: &mut CursorState,
        direction: secantus_pgplan::FetchDirection,
        count: i64,
    ) -> PgWireResult<()> {
        use secantus_pgplan::FetchDirection as Fd;
        let pos = cursor.pos;
        // (lowest, highest) 1-based row the statement may touch; `None` for
        // the highest means "to the end".
        let (lo, hi): (i64, Option<i64>) = match direction {
            Fd::Forward if count >= 0 => (
                pos.saturating_add(1),
                (count != i64::MAX).then(|| pos.saturating_add(count)),
            ),
            Fd::Forward => (pos.saturating_add(count), Some(pos - 1)),
            Fd::Backward if count >= 0 => (pos.saturating_sub(count), Some(pos - 1)),
            Fd::Backward => (
                pos.saturating_add(1),
                (count != i64::MIN).then(|| pos.saturating_sub(count)),
            ),
            Fd::Relative => {
                let t = pos.saturating_add(count);
                (t, Some(t))
            }
            Fd::Absolute if count >= 0 => (count, Some(count)),
            Fd::Absolute => (1, None),
        };
        // Back past the window: read again from the start.
        if lo >= 1 && usize::try_from(lo - 1).unwrap_or(0) < cursor.base {
            if let Some(tail) = cursor.tail.as_mut() {
                tail.scan = tail.start.clone();
                tail.exhausted = false;
                cursor.rows.clear();
                cursor.typed_rows = Some(Vec::new());
                cursor.base = 0;
            }
        }
        loop {
            let have = (cursor.base + cursor.rows.len()) as i64;
            if hi.is_some_and(|h| have >= h) || cursor.tail.as_ref().is_none_or(|t| t.exhausted) {
                return Ok(());
            }
            self.check_cancel()?;
            // Rows below what this statement touches (less the margin) are
            // not kept while reading on to the end.
            if hi.is_none() {
                let keep_from = usize::try_from(have).unwrap_or(0).saturating_sub(
                    KEEP_BEHIND.max(usize::try_from(count.unsigned_abs()).unwrap_or(usize::MAX)),
                );
                if keep_from > cursor.base {
                    let n = (keep_from - cursor.base).min(cursor.rows.len());
                    cursor.rows.drain(..n);
                    if let Some(t) = cursor.typed_rows.as_mut() {
                        t.drain(..n.min(t.len()));
                    }
                    cursor.base += n;
                }
            }
            if !self.pull_cursor(cursor)? {
                return Ok(());
            }
        }
    }

    /// The rows of `sel` streamed through the block's transaction (see
    /// `BlockScan`).
    fn block_scan_stream(
        &self,
        sel: &secantus_pgplan::Select,
        order: Vec<OrderKey>,
        dedup: Option<Vec<String>>,
    ) -> PgWireResult<futures::stream::BoxStream<'static, PgWireResult<Vec<Document>>>> {
        let primed = order.iter().any(|k| k.expr.is_some());
        let mut first = BlockScan::of(self.db(), sel, order, dedup);
        // An ORDER BY expression needs this statement's thread (see
        // `declare_streamed`): sorted now, through the block.
        if primed {
            self.in_open_transaction(|| first.prime(&self.storage, &|| self.check_cancel()))?;
        }
        let scan = Arc::new(Mutex::new(first));
        {
            let mut open = self.block_scans.lock().unwrap_or_else(|e| e.into_inner());
            open.retain(|w| w.strong_count() > 0);
            open.push(Arc::downgrade(&scan));
        }
        let txn = Arc::clone(&self.txn);
        let storage = self.storage.clone();
        let backend = self.backend.clone();
        Ok(futures::stream::unfold(Some(scan), move |state| {
            let txn = Arc::clone(&txn);
            let storage = storage.clone();
            let backend = backend.clone();
            async move {
                let scan = state?;
                let out = crate::blocking_wait(|| -> PgWireResult<Vec<Document>> {
                    let mut st = scan.lock().unwrap_or_else(|e| e.into_inner());
                    if !st.pending.is_empty() {
                        let n = st.pending.len().min(BATCH);
                        return Ok(st.pending.drain(..n).collect());
                    }
                    if st.done {
                        return Ok(Vec::new());
                    }
                    if backend.terminate.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err(PgHandler::admin_shutdown());
                    }
                    if backend.cancelled() {
                        return Err(PgHandler::query_canceled());
                    }
                    let mut guard = txn.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(handle) = guard.as_mut() else {
                        // The block ended without the drain that precedes
                        // every statement: nothing is left to read through.
                        st.done = true;
                        return Err(PgHandler::user_error(
                            "34000",
                            "portal does not exist".into(),
                        ));
                    };
                    let check = || {
                        if backend.terminate.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(PgHandler::admin_shutdown());
                        }
                        if backend.cancelled() {
                            return Err(PgHandler::query_canceled());
                        }
                        Ok(())
                    };
                    storage
                        .with_user_transaction(handle, || st.read_batch(&storage, &check))
                        .map_err(|e| PgHandler::storage_err("transaction failed", e))?
                });
                match out {
                    Ok(docs) if docs.is_empty() => None,
                    Ok(docs) => Some((Ok(docs), Some(scan))),
                    // An error ends the portal after it is reported.
                    Err(e) => Some((Err(e), None)),
                }
            }
        })
        .boxed())
    }

    /// Read the rest of every streamed portal still open in the block into
    /// memory (see `BlockScan`): another statement is about to use the
    /// block's transaction.
    pub(crate) fn drain_block_scans(&self) -> PgWireResult<()> {
        let open: Vec<Arc<Mutex<BlockScan>>> = {
            let mut open = self.block_scans.lock().unwrap_or_else(|e| e.into_inner());
            open.retain(|w| w.strong_count() > 0);
            std::mem::take(&mut *open)
                .into_iter()
                .filter_map(|w| w.upgrade())
                .collect()
        };
        let cursors_streaming = self
            .cursors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|c| c.tail.is_some());
        if open.is_empty() && !cursors_streaming {
            return Ok(());
        }
        self.in_open_transaction(|| {
            for scan in open {
                let mut st = scan.lock().unwrap_or_else(|e| e.into_inner());
                while !st.done {
                    let docs = st.read_batch(&self.storage, &|| self.check_cancel())?;
                    st.pending.extend(docs);
                }
            }
            // A streamed cursor becomes a materialised one: every row, read
            // again from the start in the DECLARE's snapshot.
            let mut cursors = self.cursors.lock().unwrap_or_else(|e| e.into_inner());
            for cursor in cursors.values_mut() {
                let Some(tail) = cursor.tail.as_mut() else {
                    continue;
                };
                tail.scan = tail.start.clone();
                tail.exhausted = false;
                cursor.rows.clear();
                cursor.typed_rows = Some(Vec::new());
                cursor.base = 0;
                while self.pull_cursor(cursor)? {}
            }
            Ok(())
        })
    }
}
