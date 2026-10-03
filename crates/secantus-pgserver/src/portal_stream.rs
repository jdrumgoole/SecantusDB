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
//!   types: no join, subquery source, series, window, DISTINCT, ORDER BY,
//!   per-row WHERE residual or computed column (each of those needs every row,
//!   or thread-local session state, before the first row can go out);
//! - with a WHERE, only when the client fetches in pieces (`max_rows > 0`):
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
        let mode = if !in_block && group && clean && self.read_committed_default() {
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

    /// The SELECT as a streamed result, or `None` to run it as before.
    pub(crate) fn try_stream_select(
        &self,
        sel: &secantus_pgplan::Select,
        env: &RowEnv,
    ) -> PgWireResult<Option<Response>> {
        let mode = self
            .stream_portal
            .swap(STREAM_NEVER, std::sync::atomic::Ordering::Relaxed);
        if mode == STREAM_NEVER || (mode == STREAM_UNFILTERED && !sel.filter.is_empty()) {
            return Ok(None);
        }
        let plain = sel.sub.is_none()
            && sel.join.is_none()
            && sel.series.is_none()
            && sel.windows.is_empty()
            && sel.order.is_empty()
            && sel.distinct == secantus_pgplan::Distinct::None
            && sel.residual.is_none()
            && sel.casts.iter().all(Option::is_none)
            && sel.offset >= 0
            && !sel.table.is_empty()
            && Self::virtual_table(&sel.table).is_none();
        if !plain {
            return Ok(None);
        }
        let Some(def) = self.lookup(&sel.table) else {
            return Ok(None);
        };
        // A sequence read as a relation, and anything of a type this
        // session resolves from its own catalog, keep the materialised path.
        let builtin = def
            .columns
            .iter()
            .all(|c| secantus_pgplan::pgtypes::oid_of_name(&c.pg_type).is_some());
        if !builtin || def.column("is_called").is_some() || def.temp {
            return Ok(None);
        }
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
        let schema = Arc::new(self.row_schema(&def, &sel.columns, &sel.casts));
        let fields: Vec<String> = sel.columns.iter().map(|(_, f)| f.clone()).collect();
        let rx = self.spawn_portal_reader(sel)?;
        let source = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        })
        .flat_map(|item| match item {
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

    /// Start the thread that reads `sel`'s rows and hands them over.
    fn spawn_portal_reader(
        &self,
        sel: &secantus_pgplan::Select,
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
            let scanned = storage.scan_matching_batches(&db, &table, &filter, BATCH, |blobs| {
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
