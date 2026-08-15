//! The cursor registry (R3) and its driving commands `getMore` / `killCursors`.
//!
//! A port of `src/secantus/cursors.py`'s `CursorRegistry`, byte-seam native:
//! a cursor's pending documents are encoded BSON (`Vec<Vec<u8>>`), as they come
//! off the storage cursor, not decoded dicts. Thread-safe behind one `Mutex`,
//! with opportunistic idle-TTL pruning on every operation and an **injectable
//! clock** so tests drive expiry deterministically. Cursor ids are 63-bit
//! random (non-tailable odd; tailable `> 2**32`) — unpredictable so a peer can't
//! enumerate or hijack cursors.
//!
//! **This slice — R3 — covers the registry plus the non-tailable getMore and
//! killCursors handlers.** The tailable (change-stream) getMore path — drain
//! buffered events, call the `producer`, block on the storage oplog condvar for
//! `awaitData`, emit `postBatchResumeToken` — is deferred to the change-stream
//! slice (it needs the oplog tail + `notify_oplog_waiters`, not in the command
//! `Storage` trait yet). Cursor *creation* (`find` / `aggregate` / `watch`) also
//! lands later; until then cursors are registered programmatically.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use bson::{doc, Bson, Document};

use crate::util::as_i64;
use crate::{
    CommandContext, CommandError, HandlerResult, DEFAULT_BATCH_SIZE, MAX_GETMORE_BATCH_BYTES,
};

/// Idle TTL for ordinary cursors — MongoDB's 10-minute default.
pub const DEFAULT_IDLE_TTL_SECONDS: f64 = 600.0;
/// Idle TTL for tailable / change-stream cursors (legitimately idle longer).
pub const TAILABLE_IDLE_TTL_SECONDS: f64 = 1800.0;
/// Hard cap on simultaneous live cursors (OOM guard against cursor floods).
pub const DEFAULT_MAX_CURSORS: usize = 10_000;

/// A cursor-registry failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorError {
    /// No live cursor with this id (expired, killed, or never existed).
    NotFound(i64),
    /// `register` would exceed `max_cursors`.
    LimitExceeded(usize),
    /// Could not mint a unique id within the attempt budget (astronomically
    /// unlikely; surfaced rather than panicked).
    MintFailed,
}

/// Produces newly-available encoded event documents for a tailable cursor.
/// Invoked by the change-stream getMore handler; the registry stores it and
/// calls `produce` when the buffered events run out.
pub trait CursorProducer: Send {
    /// Poll for new events, advancing the producer's internal oplog position.
    /// Returns the projected event documents (`bson::encode` bytes).
    fn produce(&mut self) -> Vec<Vec<u8>>;
    /// The producer's current oplog position (seq of the last entry consumed) —
    /// for the `postBatchResumeToken` and the next `awaitData` wait.
    fn position(&self) -> i64;
    /// Whether the producer has seen an invalidating event (drop / rename /
    /// dropDatabase on the watched scope). Once true, the cursor emits its
    /// buffered events then closes.
    fn invalidated(&self) -> bool;
    /// A fatal error the producer hit while projecting an event (e.g. a user
    /// `$project`/`$unset` stripped the `_id`/resume token — mongod code 280,
    /// `ChangeStreamFatalError`). Surfaced by the getMore handler as an `ok: 0`
    /// reply that ends the stream. `None` for the common case.
    fn fatal_error(&self) -> Option<CommandError> {
        None
    }
}

/// The result of draining a tailable change-stream batch:
/// `(batch, position, closed, fatal_error)`. `position` is the producer's oplog
/// seq (for the resume token); `closed` is set once the cursor is exhausted /
/// invalidated; `fatal_error` carries a getMore-time projection error (code 280).
type TailableBatch = (Vec<Vec<u8>>, i64, bool, Option<CommandError>);

/// Options for a tailable (change-stream) cursor registration.
#[derive(Default)]
pub struct TailableOptions {
    pub await_data: bool,
    pub no_cursor_timeout: bool,
    pub position_seq: i64,
    pub collection_uuid: Option<[u8; 16]>,
    /// Docs matched at creation time that didn't fit the first batch; drained
    /// before the producer is consulted.
    pub initial_remaining: Vec<Vec<u8>>,
    /// Whether this is a change stream rather than a plain capped-collection
    /// tail. A dropped collection is signalled differently for the two: a
    /// change stream drives its own invalidation through the producer's final
    /// `invalidate` event, while a plain tail gets a `QueryPlanKilled`
    /// "collection dropped" error. Mirrors `cursors._Entry.change_stream`.
    pub change_stream: bool,
}

struct Entry {
    namespace: String,
    remaining: VecDeque<Vec<u8>>,
    last_access: f64,
    tailable: bool,
    await_data: bool,
    no_cursor_timeout: bool,
    change_stream: bool,
    /// Tombstoned by `kill_namespace`: the collection was dropped out from
    /// under a plain tailable cursor, and the next getMore must say so.
    dropped: bool,
    // The tailable fields below are written by `register_tailable` but only read
    // by the change-stream getMore slice (deferred), so they're allow(dead_code)
    // until that lands.
    #[allow(dead_code)]
    producer: Option<Box<dyn CursorProducer>>,
    #[allow(dead_code)]
    position_seq: i64,
    #[allow(dead_code)]
    collection_uuid: Option<[u8; 16]>,
    invalidated: bool,
    #[allow(dead_code)]
    final_event_pending: bool,
    last_token: Option<Document>,
    /// A fatal projection error (code 280) the producer hit; once set, the next
    /// getMore returns it as an `ok: 0` reply and the cursor is dropped.
    fatal_error: Option<CommandError>,
}

/// A read-only snapshot of a cursor's routing state, so the getMore handler can
/// branch (namespace check, tailable vs not) without holding the lock. Reading
/// it bumps the cursor's `last_access`, mirroring `cursors.py::get`.
#[derive(Debug, Clone)]
pub struct CursorInfo {
    pub namespace: String,
    pub tailable: bool,
    pub await_data: bool,
    pub invalidated: bool,
    pub has_remaining: bool,
}

/// A `currentOp`-style description of a live cursor (no mutable state leaked).
#[derive(Debug, Clone, PartialEq)]
pub struct CursorSnapshot {
    pub cursor_id: i64,
    pub namespace: String,
    pub remaining: usize,
    pub last_access: f64,
    pub tailable: bool,
    pub await_data: bool,
}

struct Inner {
    cursors: HashMap<i64, Entry>,
    last_prune: f64,
}

/// Per-server thread-safe map of cursor id → pending documents.
pub struct CursorRegistry {
    inner: Mutex<Inner>,
    idle_ttl_seconds: f64,
    tailable_idle_ttl_seconds: f64,
    max_cursors: usize,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
}

impl Default for CursorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorRegistry {
    /// A registry with MongoDB-default TTLs and a monotonic clock.
    pub fn new() -> Self {
        let base = Instant::now();
        Self::with_clock(
            Box::new(move || base.elapsed().as_secs_f64()),
            DEFAULT_IDLE_TTL_SECONDS,
            TAILABLE_IDLE_TTL_SECONDS,
            DEFAULT_MAX_CURSORS,
        )
    }

    /// A registry with an injected clock (for deterministic TTL tests).
    pub fn with_clock(
        clock: Box<dyn Fn() -> f64 + Send + Sync>,
        idle_ttl_seconds: f64,
        tailable_idle_ttl_seconds: f64,
        max_cursors: usize,
    ) -> Self {
        CursorRegistry {
            inner: Mutex::new(Inner {
                cursors: HashMap::new(),
                last_prune: f64::NEG_INFINITY,
            }),
            idle_ttl_seconds,
            tailable_idle_ttl_seconds,
            max_cursors,
            clock,
        }
    }

    /// Register an ordinary cursor over already-fetched documents; returns its
    /// id. The remaining docs drain via `getMore`.
    pub fn register(&self, namespace: &str, remaining: Vec<Vec<u8>>) -> Result<i64, CursorError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        if inner.cursors.len() >= self.max_cursors {
            return Err(CursorError::LimitExceeded(self.max_cursors));
        }
        let id = mint_id(&inner.cursors, false)?;
        let now = (self.clock)();
        inner.cursors.insert(
            id,
            Entry {
                namespace: namespace.to_string(),
                remaining: remaining.into_iter().collect(),
                last_access: now,
                tailable: false,
                change_stream: false,
                dropped: false,
                await_data: false,
                no_cursor_timeout: false,
                producer: None,
                position_seq: 0,
                collection_uuid: None,
                invalidated: false,
                final_event_pending: false,
                last_token: None,
                fatal_error: None,
            },
        );
        Ok(id)
    }

    /// Register a tailable (change-stream) cursor backed by a producer closure.
    /// Ids are `> 2**32` to match what `mongod` issues for change streams.
    pub fn register_tailable(
        &self,
        namespace: &str,
        producer: Box<dyn CursorProducer>,
        opts: TailableOptions,
    ) -> Result<i64, CursorError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        if inner.cursors.len() >= self.max_cursors {
            return Err(CursorError::LimitExceeded(self.max_cursors));
        }
        let id = mint_id(&inner.cursors, true)?;
        let now = (self.clock)();
        inner.cursors.insert(
            id,
            Entry {
                namespace: namespace.to_string(),
                remaining: opts.initial_remaining.into_iter().collect(),
                last_access: now,
                tailable: true,
                change_stream: opts.change_stream,
                dropped: false,
                await_data: opts.await_data,
                no_cursor_timeout: opts.no_cursor_timeout,
                producer: Some(producer),
                position_seq: opts.position_seq,
                collection_uuid: opts.collection_uuid,
                invalidated: false,
                final_event_pending: false,
                last_token: None,
                fatal_error: None,
            },
        );
        Ok(id)
    }

    /// Snapshot a cursor's routing state, bumping its `last_access`
    /// (`cursors.py::get`).
    pub fn info(&self, cursor_id: i64) -> Result<CursorInfo, CursorError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        let now = (self.clock)();
        let e = inner
            .cursors
            .get_mut(&cursor_id)
            .ok_or(CursorError::NotFound(cursor_id))?;
        e.last_access = now;
        Ok(CursorInfo {
            namespace: e.namespace.clone(),
            tailable: e.tailable,
            await_data: e.await_data,
            invalidated: e.invalidated,
            has_remaining: !e.remaining.is_empty(),
        })
    }

    /// Drain up to `batch_size` documents (`<= 0` ⇒ all remaining), never more
    /// than `max_bytes` of encoded BSON (but always at least one document, so a
    /// drain makes progress). Returns the batch and whether the cursor is now
    /// exhausted. An exhausted *non-tailable* cursor is removed; a tailable
    /// cursor persists across empty batches and always reports
    /// `exhausted == false` (`cursors.py::next_batch`).
    pub fn next_batch(
        &self,
        cursor_id: i64,
        batch_size: i64,
        max_bytes: usize,
    ) -> Result<(Vec<Vec<u8>>, bool), CursorError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        let now = (self.clock)();
        let (batch, exhausted, tailable) = {
            let e = inner
                .cursors
                .get_mut(&cursor_id)
                .ok_or(CursorError::NotFound(cursor_id))?;
            let want = if batch_size <= 0 {
                e.remaining.len()
            } else {
                batch_size as usize
            };
            let mut take = 0usize;
            let mut bytes = 0usize;
            for blob in e.remaining.iter().take(want.min(e.remaining.len())) {
                if take > 0 && bytes + blob.len() > max_bytes {
                    break;
                }
                bytes += blob.len();
                take += 1;
            }
            let batch: Vec<Vec<u8>> = e.remaining.drain(..take).collect();
            let exhausted = e.remaining.is_empty();
            if e.tailable || !exhausted {
                e.last_access = now;
            }
            (batch, exhausted, e.tailable)
        };
        if tailable {
            return Ok((batch, false));
        }
        if exhausted {
            inner.cursors.remove(&cursor_id);
        }
        Ok((batch, exhausted))
    }

    /// Drain the next batch for a TAILABLE (change-stream) cursor. R3b-a is
    /// non-blocking: when the buffer is empty the producer is polled once, then
    /// up to `batch_size` events are drained. Returns `(batch, position, closed)`
    /// — `position` is the producer's current oplog seq (for the resume token)
    /// and `closed` is true once an invalidating event (or a `killCursors` /
    /// drop-driven invalidation) has been delivered AND the buffer is empty, at
    /// which point the cursor is removed and the handler reports id 0.
    pub fn tailable_next_batch(
        &self,
        cursor_id: i64,
        batch_size: i64,
    ) -> Result<TailableBatch, CursorError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        let now = (self.clock)();
        let e = inner
            .cursors
            .get_mut(&cursor_id)
            .ok_or(CursorError::NotFound(cursor_id))?;
        e.last_access = now;
        // Poll the producer once when nothing is buffered and the cursor hasn't
        // been invalidated (by an oplog drop/rename or a concurrent killCursors).
        if e.remaining.is_empty() && !e.invalidated && e.fatal_error.is_none() {
            if let Some(p) = e.producer.as_mut() {
                let events = p.produce();
                e.position_seq = p.position();
                if p.invalidated() {
                    e.invalidated = true;
                }
                if let Some(err) = p.fatal_error() {
                    e.fatal_error = Some(err);
                }
                e.remaining.extend(events);
            }
        }
        let want = if batch_size <= 0 {
            e.remaining.len()
        } else {
            batch_size as usize
        };
        let take = want.min(e.remaining.len());
        let batch: Vec<Vec<u8>> = e.remaining.drain(..take).collect();
        let position = e.position_seq;
        // A fatal projection error (code 280) ends the stream once any buffered
        // events have drained; surface it (and drop the cursor) only when empty.
        let fatal = if e.remaining.is_empty() {
            e.fatal_error.clone()
        } else {
            None
        };
        let closed = (e.invalidated || fatal.is_some()) && e.remaining.is_empty();
        if closed {
            inner.cursors.remove(&cursor_id);
        }
        Ok((batch, position, closed, fatal))
    }

    /// Kill the given cursors, returning `(killed, not_found)`.
    pub fn kill(&self, cursor_ids: &[i64]) -> (Vec<i64>, Vec<i64>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        let mut killed = Vec::new();
        let mut not_found = Vec::new();
        for &cid in cursor_ids {
            if inner.cursors.remove(&cid).is_some() {
                killed.push(cid);
            } else {
                not_found.push(cid);
            }
        }
        (killed, not_found)
    }

    /// Kill every cursor open on `namespace` (a `db.coll` string), returning the
    /// count. mongod kills a collection's cursors when it's dropped or renamed;
    /// SecantusDB's cursors hold detached snapshots, so without this they'd keep
    /// serving rows after the collection is gone.
    ///
    /// The three kinds are treated differently, mirroring
    /// `cursors.kill_namespace`:
    ///
    /// * **Non-tailable** — removed outright, so the next `getMore` is
    ///   `CursorNotFound` (mongo-c-driver's `error_document/getmore`).
    /// * **Plain tailable** — *tombstoned* (`dropped = true`, entry kept) so the
    ///   next `getMore` can return `QueryPlanKilled` "collection dropped", which
    ///   is what mongod tells a tailing client and what mongo-php-driver's
    ///   `cursor-tailable_error-001` asserts on. A bare `CursorNotFound` doesn't
    ///   say why the tail ended.
    /// * **Change streams** — left alone: they drive their own drop/rename
    ///   invalidation through the producer's final `invalidate` event, and
    ///   tombstoning would replace that event with an error.
    pub fn kill_namespace(&self, namespace: &str) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let affected: Vec<i64> = inner
            .cursors
            .iter()
            .filter(|(_, e)| e.namespace == namespace && !e.change_stream)
            .map(|(&cid, _)| cid)
            .collect();
        for cid in &affected {
            let tailable = inner.cursors.get(cid).is_some_and(|e| e.tailable);
            if tailable {
                if let Some(e) = inner.cursors.get_mut(cid) {
                    e.dropped = true;
                }
            } else {
                inner.cursors.remove(cid);
            }
        }
        affected.len()
    }

    /// Remember the token of the last event handed to the client, so a later
    /// empty batch can re-emit it instead of rewinding to a positional
    /// high-water mark.
    pub fn remember_last_token(&self, cursor_id: i64, tok: &Bson) {
        if let Some(d) = tok.as_document() {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(e) = inner.cursors.get_mut(&cursor_id) {
                e.last_token = Some(d.clone());
            }
        }
    }

    /// The last event token handed to this cursor's client, if any.
    pub fn last_token(&self, cursor_id: i64) -> Option<Bson> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .cursors
            .get(&cursor_id)
            .and_then(|e| e.last_token.clone())
            .map(Bson::Document)
    }

    /// Whether this cursor was tombstoned by a drop/rename of its collection.
    pub fn was_dropped(&self, cursor_id: i64) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.cursors.get(&cursor_id).is_some_and(|e| e.dropped)
    }

    /// Mark a cursor invalidated (a blocked tailable getMore wakes and ends).
    /// No-op if the cursor is gone.
    pub fn invalidate(&self, cursor_id: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = inner.cursors.get_mut(&cursor_id) {
            e.invalidated = true;
        }
    }

    /// Number of live cursors (after a prune pass).
    pub fn len(&self) -> usize {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        inner.cursors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Describe the live cursors (for `currentOp` / admin), sorted by id.
    pub fn snapshot(&self) -> Vec<CursorSnapshot> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune_locked(&mut inner);
        let mut out: Vec<CursorSnapshot> = inner
            .cursors
            .iter()
            .map(|(id, e)| CursorSnapshot {
                cursor_id: *id,
                namespace: e.namespace.clone(),
                remaining: e.remaining.len(),
                last_access: e.last_access,
                tailable: e.tailable,
                await_data: e.await_data,
            })
            .collect();
        out.sort_by_key(|s| s.cursor_id);
        out
    }

    /// The prune cadence — capped at 60s and a tenth of the TTL so a cursor
    /// can't outlive its TTL by more than ~10% (`cursors.py::_prune_interval`).
    fn prune_interval(&self) -> f64 {
        let ttl = self.idle_ttl_seconds;
        if ttl <= 0.0 {
            return f64::INFINITY;
        }
        60.0_f64.min(ttl / 10.0)
    }

    /// Drop cursors idle past their TTL, no more than once per prune interval.
    fn prune_locked(&self, inner: &mut Inner) {
        let now = (self.clock)();
        if now - inner.last_prune < self.prune_interval() {
            return;
        }
        let idle = self.idle_ttl_seconds;
        let tailable_idle = self.tailable_idle_ttl_seconds;
        inner.cursors.retain(|_cid, e| {
            if e.no_cursor_timeout {
                return true;
            }
            let ttl = if e.tailable { tailable_idle } else { idle };
            if ttl <= 0.0 {
                return true;
            }
            e.last_access >= now - ttl
        });
        inner.last_prune = now;
    }
}

/// Mint a unique cursor id: 63-bit random, odd for ordinary cursors, `> 2**32`
/// for tailable ones (`cursors.py::register` / `register_tailable`).
fn mint_id(cursors: &HashMap<i64, Entry>, tailable: bool) -> Result<i64, CursorError> {
    for _ in 0..8 {
        let bits = rand::random::<u64>() >> 1; // clear the sign bit → positive i64
        let candidate = if tailable {
            bits | (1u64 << 32)
        } else {
            bits | 1
        } as i64;
        if !cursors.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    Err(CursorError::MintFailed)
}

// --- command handlers ----------------------------------------------------

/// `getMore` — pull the next batch from a cursor. Non-tailable path; the
/// tailable change-stream path is deferred (see the module docs).
pub fn get_more(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let cursor_id = doc
        .get("getMore")
        .and_then(as_i64)
        .ok_or_else(|| CommandError::new(2, "BadValue", "getMore requires a cursor id"))?;
    let coll = doc.get("collection").and_then(Bson::as_str).unwrap_or("");
    // Tri-state like `find`'s, but the absent-default differs: mongod's 101
    // applies only to the FIRST batch — an unspecified getMore batchSize means
    // "fill up to 16MB". Only the tailable path keeps the small default (its
    // events arrive incrementally off the oplog).
    let batch_size = doc.get("batchSize").and_then(as_i64).filter(|&b| b > 0);
    let ns = format!("{}.{}", ctx.db_name, coll);
    let cursors = ctx.cursors()?;

    let info = match cursors.info(cursor_id) {
        Ok(i) => i,
        Err(_) => return Ok(cursor_not_found(cursor_id)),
    };
    // Ownership check: the caller's claimed namespace must match the cursor's,
    // else we answer CursorNotFound (don't confirm cursor ids across conns).
    if !info.namespace.is_empty() && ns != info.namespace {
        return Ok(cursor_not_found(cursor_id));
    }
    // The collection was dropped out from under a plain tailable cursor: say so
    // before anything else, since any buffered rows are stale. mongod kills the
    // plan executor with QueryPlanKilled (175) and names the namespace;
    // mongo-php-driver's `cursor-tailable_error-001` asserts the message
    // mentions "collection dropped" rather than a bare CursorNotFound, which
    // wouldn't tell a tailing client why its tail ended.
    if cursors.was_dropped(cursor_id) {
        cursors.kill(&[cursor_id]);
        return Ok(CommandError::new(
            175,
            "QueryPlanKilled",
            format!("collection dropped: {}", info.namespace),
        )
        .into_reply());
    }
    if info.tailable {
        let storage = ctx.storage()?;
        // awaitData: block until an event arrives or maxTimeMS elapses, instead
        // of busy-polling. pymongo doesn't always send maxTimeMS on change-stream
        // getMore, so default to 1s (also lets the connection thread be reaped on
        // shutdown). A non-await_data cursor or a zero deadline polls exactly once.
        let max_time_ms = doc.get("maxTimeMS").and_then(as_i64).unwrap_or(1000).max(0) as u64;
        let deadline = Instant::now() + Duration::from_millis(max_time_ms);

        let mut batch;
        let mut position;
        let mut closed;
        let mut fatal;
        loop {
            match cursors
                .tailable_next_batch(cursor_id, batch_size.unwrap_or(DEFAULT_BATCH_SIZE as i64))
            {
                Ok((b, p, c, f)) => {
                    batch = b;
                    position = p;
                    closed = c;
                    fatal = f;
                }
                Err(_) => return Ok(cursor_not_found(cursor_id)),
            }
            // A fatal projection error (e.g. the user pipeline stripped the
            // resume-token `_id`) ends the stream immediately — don't block.
            if fatal.is_some() {
                break;
            }
            if !batch.is_empty() || closed || !info.await_data {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            // Block on the storage oplog condvar until it advances past
            // `position` or the deadline; then re-poll (the producer reads the
            // newly-appended entries). killCursors wakes this via notify.
            storage.wait_for_oplog(position, (deadline - now).as_millis() as u64);
        }

        // A fatal projection error ends the stream with an `ok: 0` reply (the
        // cursor was already dropped in the registry).
        if let Some(err) = fatal {
            return Ok(err.into_reply());
        }

        // postBatchResumeToken: the last event's token when this batch carried
        // events. On an EMPTY batch a high-water-mark token lets a client resume
        // past a quiet getMore — but only if it is actually newer than the last
        // event we delivered. mongod does not rewind: when nothing has happened
        // since the last event, the token stays that event's, and mongocxx's
        // "must continuously track the last seen resumeToken" asserts exactly
        // that (its final read is empty and must still equal the previous
        // token). Emitting a fresh high-water token there replaced a real token
        // — carrying its `ns` and `documentKey` — with a positional one whose
        // both fields are empty.
        let pbrt = match post_batch_resume_token(&batch) {
            Some(tok) => {
                cursors.remember_last_token(cursor_id, &tok);
                Some(tok)
            }
            None => {
                let bytes = storage.high_water_mark_token(position);
                let hwm = if bytes.is_empty() {
                    None
                } else {
                    Document::from_reader(&mut bytes.as_slice())
                        .ok()
                        .map(Bson::Document)
                };
                match (hwm, cursors.last_token(cursor_id)) {
                    // Only move forward: a high-water mark at or behind the
                    // last delivered event would rewind the client's token.
                    (Some(h), Some(l)) if token_seq(&h) > token_seq(&l) => Some(h),
                    (_, Some(l)) => Some(l),
                    (h, None) => h,
                }
            }
        };
        let mut cursor_doc = doc! {
            "id": Bson::Int64(if closed { 0 } else { cursor_id }),
            "ns": ns,
        };
        if let Some(tok) = pbrt {
            cursor_doc.insert("postBatchResumeToken", tok);
        }
        // Raw splice, same as the plain getMore below: the event blobs go to
        // the wire encoder undecoded. The postBatchResumeToken only ever
        // needed the LAST blob (`post_batch_resume_token`), so nothing here
        // requires materialising the whole batch any more; the splice keeps
        // the exact old field order (nextBatch, id, ns, postBatchResumeToken).
        ctx.pending_batch = Some(crate::PendingBatch {
            batch_field: "nextBatch",
            batch,
        });
        return Ok(doc! { "cursor": cursor_doc, "ok": 1.0 });
    }

    let (batch, exhausted) =
        match cursors.next_batch(cursor_id, batch_size.unwrap_or(0), MAX_GETMORE_BATCH_BYTES) {
            Ok(x) => x,
            Err(_) => return Ok(cursor_not_found(cursor_id)),
        };
    // The registry already holds the batch as pre-encoded blobs; hand them to
    // the server (`ctx.pending_batch`) to splice onto the wire without the
    // decode→re-encode round-trip `docs_to_bson` would cost. The reply carries
    // only the cursor envelope. (The tailable branch above splices the same
    // way; its postBatchResumeToken decodes only the last blob.)
    ctx.pending_batch = Some(crate::PendingBatch {
        batch_field: "nextBatch",
        batch,
    });
    Ok(doc! {
        "cursor": {
            "id": Bson::Int64(if exhausted { 0 } else { cursor_id }),
            "ns": ns,
        },
        "ok": 1.0,
    })
}

/// `killCursors` — explicitly close cursors.
pub fn kill_cursors(doc: &Document, ctx: &mut CommandContext) -> HandlerResult {
    let cursor_ids: Vec<i64> = match doc.get("cursors") {
        Some(Bson::Array(a)) => a.iter().filter_map(as_i64).collect(),
        _ => Vec::new(),
    };
    // A blocked tailable getMore waits on the STORAGE oplog condvar, not the
    // registry, so wake it via the storage's notify (best-effort: a cursorless
    // or storageless context just skips it).
    let storage = ctx.storage.clone();
    let cursors = ctx.cursors()?;
    // Invalidate + remove first so the woken getMore re-polls and sees the
    // cursor gone (CursorNotFound) or invalidated, rather than reporting it
    // still alive after the client killed it.
    for &cid in &cursor_ids {
        cursors.invalidate(cid);
    }
    let (killed, not_found) = cursors.kill(&cursor_ids);
    if let Some(s) = storage {
        s.notify_oplog_waiters();
    }
    Ok(doc! {
        "cursorsKilled": int64_array(killed),
        "cursorsNotFound": int64_array(not_found),
        "cursorsAlive": Vec::<Bson>::new(),
        "cursorsUnknown": Vec::<Bson>::new(),
        "ok": 1.0,
    })
}

fn cursor_not_found(cursor_id: i64) -> Document {
    CommandError::new(
        43,
        "CursorNotFound",
        format!("cursor id {cursor_id} not found"),
    )
    .into_reply()
}

fn int64_array(ids: Vec<i64>) -> Vec<Bson> {
    ids.into_iter().map(Bson::Int64).collect()
}

/// The `postBatchResumeToken` for a tailable batch: the `_id` (resume token) of
/// the last event in the batch. `None` for an empty batch — R3b-a omits the
/// PBRT then; pymongo falls back to the last seen event's `_id`, which is
/// correct when no new events were delivered. Empty-batch high-water-mark
/// advancement (and noop-heartbeat tracking) is R3b-b.
/// The `s` (oplog seq) inside a `{"_data": "<hex>"}` resume token, or -1 when it
/// can't be read — an unreadable token must never look newer than a real one.
fn token_seq(tok: &Bson) -> i64 {
    let Some(d) = tok.as_document() else {
        return -1;
    };
    let Ok(hex) = d.get_str("_data") else {
        return -1;
    };
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
        .collect();
    Document::from_reader(&mut bytes.as_slice())
        .ok()
        .and_then(|d| d.get_i64("s").ok())
        .unwrap_or(-1)
}

fn post_batch_resume_token(batch: &[Vec<u8>]) -> Option<Bson> {
    let last = batch.last()?;
    let doc = Document::from_reader(&mut last.as_slice()).ok()?;
    doc.get("_id").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;
    use std::sync::{Arc, Mutex as StdMutex};

    fn enc(d: &Document) -> Vec<u8> {
        let mut v = Vec::new();
        d.to_writer(&mut v).unwrap();
        v
    }

    fn manual_clock() -> (Arc<StdMutex<f64>>, Box<dyn Fn() -> f64 + Send + Sync>) {
        let t = Arc::new(StdMutex::new(0.0));
        let t2 = t.clone();
        (
            t,
            Box::new(move || *t2.lock().unwrap_or_else(|e| e.into_inner())),
        )
    }

    #[test]
    fn register_and_drain_in_batches() {
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..5).map(|i| enc(&doc! {"_id": i})).collect();
        let id = reg.register("t.c", docs).unwrap();
        assert_eq!(reg.len(), 1);
        let (b1, ex1) = reg.next_batch(id, 2, usize::MAX).unwrap();
        assert_eq!(b1.len(), 2);
        assert!(!ex1);
        let (b2, ex2) = reg.next_batch(id, 2, usize::MAX).unwrap();
        assert_eq!(b2.len(), 2);
        assert!(!ex2);
        let (b3, ex3) = reg.next_batch(id, 2, usize::MAX).unwrap();
        assert_eq!(b3.len(), 1);
        assert!(ex3, "exhausted on the final partial batch");
        // Exhausted non-tailable cursor is gone.
        assert_eq!(reg.len(), 0);
        assert_eq!(
            reg.next_batch(id, 2, usize::MAX),
            Err(CursorError::NotFound(id))
        );
    }

    #[test]
    fn batch_size_zero_drains_all() {
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..3).map(|i| enc(&doc! {"_id": i})).collect();
        let id = reg.register("t.c", docs).unwrap();
        let (b, ex) = reg.next_batch(id, 0, usize::MAX).unwrap();
        assert_eq!(b.len(), 3);
        assert!(ex);
    }

    #[test]
    fn byte_budget_caps_a_batch_but_always_makes_progress() {
        let reg = CursorRegistry::new();
        let docs: Vec<Vec<u8>> = (0..4)
            .map(|i| enc(&doc! {"_id": i, "pad": "x".repeat(100)}))
            .collect();
        let per_doc = docs[0].len();
        let id = reg.register("t.c", docs).unwrap();
        // Budget for two docs: the drain stops before the third.
        let (b1, ex1) = reg.next_batch(id, 0, per_doc * 2).unwrap();
        assert_eq!(b1.len(), 2);
        assert!(!ex1);
        // A budget smaller than one document still yields one doc (progress).
        let (b2, ex2) = reg.next_batch(id, 0, 1).unwrap();
        assert_eq!(b2.len(), 1);
        assert!(!ex2);
        // Explicit count limit still byte-capped.
        let (b3, ex3) = reg.next_batch(id, 5, per_doc).unwrap();
        assert_eq!(b3.len(), 1);
        assert!(ex3);
    }

    #[test]
    fn kill_reports_killed_and_not_found() {
        let reg = CursorRegistry::new();
        let id = reg.register("t.c", vec![enc(&doc! {"_id": 1})]).unwrap();
        let (killed, not_found) = reg.kill(&[id, 999]);
        assert_eq!(killed, vec![id]);
        assert_eq!(not_found, vec![999]);
    }

    #[test]
    fn ids_are_positive_and_odd_for_regular() {
        let reg = CursorRegistry::new();
        for _ in 0..50 {
            let id = reg.register("t.c", vec![]).unwrap();
            assert!(id > 0);
            assert_eq!(id & 1, 1, "regular ids are odd");
        }
    }

    #[test]
    fn tailable_ids_above_2_pow_32_and_persist_on_empty() {
        struct Noop;
        impl CursorProducer for Noop {
            fn produce(&mut self) -> Vec<Vec<u8>> {
                vec![]
            }
            fn position(&self) -> i64 {
                0
            }
            fn invalidated(&self) -> bool {
                false
            }
        }
        let reg = CursorRegistry::new();
        let id = reg
            .register_tailable("t.c", Box::new(Noop), TailableOptions::default())
            .unwrap();
        assert!(id > (1i64 << 32));
        // Draining a tailable cursor never exhausts/removes it.
        let (b, ex) = reg.next_batch(id, 10, usize::MAX).unwrap();
        assert!(b.is_empty());
        assert!(!ex);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn tailable_next_batch_polls_producer_and_advances() {
        // A producer that yields two events on the first poll (advancing to
        // seq 5, then flags invalidated), then nothing.
        struct Two {
            calls: u32,
        }
        impl CursorProducer for Two {
            fn produce(&mut self) -> Vec<Vec<u8>> {
                self.calls += 1;
                if self.calls == 1 {
                    vec![enc(&doc! {"_id": "a", "operationType": "insert"})]
                } else {
                    vec![]
                }
            }
            fn position(&self) -> i64 {
                if self.calls >= 1 {
                    5
                } else {
                    0
                }
            }
            fn invalidated(&self) -> bool {
                false
            }
        }
        let reg = CursorRegistry::new();
        let id = reg
            .register_tailable(
                "d.c",
                Box::new(Two { calls: 0 }),
                TailableOptions::default(),
            )
            .unwrap();
        // First poll produces the event; position advances; cursor stays open.
        let (batch, pos, closed, _fatal) = reg.tailable_next_batch(id, 10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(pos, 5);
        assert!(!closed);
        // Second poll: nothing new, cursor persists (not exhausted).
        let (batch2, _pos2, closed2, _f2) = reg.tailable_next_batch(id, 10).unwrap();
        assert!(batch2.is_empty());
        assert!(!closed2);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn tailable_closes_after_invalidation_drains() {
        // Producer flags invalidated immediately, yielding one final event.
        struct Inv;
        impl CursorProducer for Inv {
            fn produce(&mut self) -> Vec<Vec<u8>> {
                vec![enc(&doc! {"_id": "x", "operationType": "invalidate"})]
            }
            fn position(&self) -> i64 {
                9
            }
            fn invalidated(&self) -> bool {
                true
            }
        }
        let reg = CursorRegistry::new();
        let id = reg
            .register_tailable("d.c", Box::new(Inv), TailableOptions::default())
            .unwrap();
        // The invalidate event is delivered; buffer now empty + invalidated =>
        // the cursor closes (id 0) and is removed.
        let (batch, _pos, closed, _fatal) = reg.tailable_next_batch(id, 10).unwrap();
        assert_eq!(batch.len(), 1);
        assert!(closed);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn idle_cursors_pruned_after_ttl() {
        let (clock, f) = manual_clock();
        let reg = CursorRegistry::with_clock(f, 100.0, 200.0, 10_000);
        let id = reg.register("t.c", vec![enc(&doc! {"_id": 1})]).unwrap();
        // Advance past the TTL; the next operation prunes it.
        *clock.lock().unwrap_or_else(|e| e.into_inner()) = 1000.0;
        assert_eq!(reg.len(), 0);
        assert_eq!(
            reg.next_batch(id, 1, usize::MAX),
            Err(CursorError::NotFound(id))
        );
    }

    #[test]
    fn limit_exceeded() {
        let reg = CursorRegistry::with_clock(Box::new(|| 0.0), 600.0, 1800.0, 2);
        reg.register("t.c", vec![]).unwrap();
        reg.register("t.c", vec![]).unwrap();
        assert_eq!(
            reg.register("t.c", vec![]),
            Err(CursorError::LimitExceeded(2))
        );
    }

    fn ctx_with_cursors(reg: Arc<CursorRegistry>) -> CommandContext {
        CommandContext::new(1).with_cursors(reg)
    }

    #[test]
    fn get_more_command_drains_and_signals_exhaustion() {
        let reg = Arc::new(CursorRegistry::new());
        let docs: Vec<Vec<u8>> = (0..3).map(|i| enc(&doc! {"_id": i})).collect();
        let id = reg.register("t.c", docs).unwrap();
        let mut c = ctx_with_cursors(reg);
        c.db_name = "t".into();

        // Non-tailable getMore hands the batch to the server via
        // `ctx.pending_batch` (pre-encoded blobs, spliced onto the wire) rather
        // than an owned `nextBatch` array in the reply; the reply carries only
        // the cursor envelope (`id` / `ns`).
        let reply = dispatch(
            &doc! {"getMore": id, "collection": "c", "batchSize": 2},
            &mut c,
        );
        let cur = reply.get_document("cursor").unwrap();
        assert!(
            !cur.contains_key("nextBatch"),
            "batch goes to pending_batch"
        );
        let pending = c
            .pending_batch
            .as_ref()
            .expect("getMore sets pending_batch");
        assert_eq!(pending.batch_field, "nextBatch");
        assert_eq!(pending.batch.len(), 2);
        assert_eq!(
            cur.get_i64("id").unwrap(),
            id,
            "more to come ⇒ id preserved"
        );
        assert_eq!(cur.get_str("ns").unwrap(), "t.c");

        let reply = dispatch(
            &doc! {"getMore": id, "collection": "c", "batchSize": 2},
            &mut c,
        );
        let cur = reply.get_document("cursor").unwrap();
        assert_eq!(c.pending_batch.as_ref().unwrap().batch.len(), 1);
        assert_eq!(cur.get_i64("id").unwrap(), 0, "exhausted ⇒ id 0");
    }

    #[test]
    fn get_more_unknown_cursor_is_cursor_not_found() {
        let reg = Arc::new(CursorRegistry::new());
        let mut c = ctx_with_cursors(reg);
        let reply = dispatch(&doc! {"getMore": 12345_i64, "collection": "c"}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 43);
        assert_eq!(reply.get_str("codeName").unwrap(), "CursorNotFound");
    }

    #[test]
    fn get_more_wrong_namespace_is_cursor_not_found() {
        let reg = Arc::new(CursorRegistry::new());
        let id = reg.register("t.c", vec![enc(&doc! {"_id": 1})]).unwrap();
        let mut c = ctx_with_cursors(reg);
        c.db_name = "t".into();
        // claim collection "other" ⇒ ns mismatch ⇒ CursorNotFound
        let reply = dispatch(&doc! {"getMore": id, "collection": "other"}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 43);
    }

    #[test]
    fn kill_namespace_drops_matching_cursors() {
        // kill_namespace removes only cursors on the exact ns; a later getMore on
        // a killed cursor is CursorNotFound (43). Guards the drop/rename → getMore
        // path the mongo-c-driver error_document/getmore test exercises.
        let reg = Arc::new(CursorRegistry::new());
        let a = reg
            .register("t.c", vec![enc(&doc! {"_id": 1}), enc(&doc! {"_id": 2})])
            .unwrap();
        let b = reg
            .register("t.other", vec![enc(&doc! {"_id": 1})])
            .unwrap();
        assert_eq!(reg.kill_namespace("t.c"), 1);
        let mut c = ctx_with_cursors(reg);
        c.db_name = "t".into();
        // The dropped-ns cursor is gone → CursorNotFound.
        let reply = dispatch(&doc! {"getMore": a, "collection": "c"}, &mut c);
        assert_eq!(reply.get_i32("code").unwrap(), 43);
        // The other-ns cursor survives.
        let reply = dispatch(&doc! {"getMore": b, "collection": "other"}, &mut c);
        assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    }

    #[test]
    fn kill_cursors_command() {
        let reg = Arc::new(CursorRegistry::new());
        let id = reg.register("t.c", vec![enc(&doc! {"_id": 1})]).unwrap();
        let mut c = ctx_with_cursors(reg.clone());
        let reply = dispatch(&doc! {"killCursors": "c", "cursors": [id, 42_i64]}, &mut c);
        assert_eq!(
            reply.get_array("cursorsKilled").unwrap(),
            &vec![Bson::Int64(id)]
        );
        assert_eq!(
            reply.get_array("cursorsNotFound").unwrap(),
            &vec![Bson::Int64(42)]
        );
        assert_eq!(reg.len(), 0);
    }
}

#[cfg(test)]
mod drop_tombstone_tests {
    use super::*;

    struct Noop;
    impl CursorProducer for Noop {
        fn produce(&mut self) -> Vec<Vec<u8>> {
            vec![]
        }
        fn position(&self) -> i64 {
            0
        }
        fn invalidated(&self) -> bool {
            false
        }
    }

    fn tailable(reg: &CursorRegistry, ns: &str, change_stream: bool) -> i64 {
        reg.register_tailable(
            ns,
            Box::new(Noop),
            TailableOptions {
                change_stream,
                ..Default::default()
            },
        )
        .unwrap()
    }

    /// A plain tail is tombstoned, not removed: the next getMore has to be able
    /// to say *why* the tail ended. mongo-php-driver's
    /// `cursor-tailable_error-001` asserts on "collection dropped", which a
    /// bare CursorNotFound cannot convey.
    #[test]
    fn a_dropped_collection_tombstones_a_plain_tailable_cursor() {
        let reg = CursorRegistry::new();
        let cid = tailable(&reg, "t.c", false);
        assert_eq!(reg.kill_namespace("t.c"), 1);
        assert!(
            reg.was_dropped(cid),
            "the entry must survive, flagged dropped"
        );
        assert!(reg.info(cid).is_ok(), "tombstoned, not removed");
    }

    /// A non-tailable cursor is removed outright — a later getMore is
    /// CursorNotFound (mongo-c-driver's `error_document/getmore`).
    #[test]
    fn a_dropped_collection_removes_a_non_tailable_cursor() {
        let reg = CursorRegistry::new();
        let cid = reg.register("t.c", vec![vec![1, 2, 3]]).unwrap();
        assert_eq!(reg.kill_namespace("t.c"), 1);
        assert!(reg.info(cid).is_err(), "removed, not tombstoned");
        assert!(!reg.was_dropped(cid));
    }

    /// Change streams are left alone: they signal a drop through their own
    /// final `invalidate` event, and a tombstone would replace that event with
    /// an error.
    #[test]
    fn a_dropped_collection_leaves_change_streams_alone() {
        let reg = CursorRegistry::new();
        let cid = tailable(&reg, "t.c", true);
        assert_eq!(
            reg.kill_namespace("t.c"),
            0,
            "change streams are not counted"
        );
        assert!(!reg.was_dropped(cid), "no tombstone on a change stream");
        assert!(reg.info(cid).is_ok(), "and it stays alive");
    }

    /// Only the dropped namespace is affected.
    #[test]
    fn other_namespaces_are_untouched() {
        let reg = CursorRegistry::new();
        let other = tailable(&reg, "t.other", false);
        assert_eq!(reg.kill_namespace("t.c"), 0);
        assert!(!reg.was_dropped(other));
    }
}
