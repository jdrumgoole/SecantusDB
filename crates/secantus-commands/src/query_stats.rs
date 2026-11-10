//! mongod's query stats store, which `$queryStats` lists (RM-215).
//!
//! Once `internalQueryStatsRateLimit` is not 0 (its default; -1 is every
//! query, `n` a sliding window of `n` a second), mongod 8.2.11 records each
//! `find`, `aggregate`, `distinct` and `count` that succeeds under its
//! *key*: the query shape (`query_shape::stats_shape`) and what else the
//! command and its client said -- `comment`, `readConcern`, the API
//! parameters, the client's metadata, the kind of collection, `hint`,
//! `maxTimeMS` and the command's own options. An entry is found by the
//! SHA-256 of the key with representative literals (`keyHash`, base64),
//! and printed with debug type strings (`?number`). Transcribed from 8.2.11's
//! `query_stats/` (`key.cpp`, `find_key.cpp`, `agg_key.cpp`,
//! `query_stats.cpp`, `query_stats_entry.cpp`, `rate_limiting.cpp`) and
//! checked against mongod (`tests/mongod_live/test_query_stats.py`).
//!
//! A command that leaves a cursor open is recorded when the cursor closes
//! -- exhausted, killed, timed out or invalidated -- with what its
//! `getMore`s added; a change stream's cursor, which never exhausts, at
//! each of its operations. A command that fails is not recorded; a cursor
//! a `getMore` fails on is, as it closes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use bson::{doc, Bson, DateTime, Decimal128, Document};
use sha2::{Digest, Sha256};

use crate::query_shape::StatsShape;
use crate::CommandContext;

/// The store, one per server.
#[derive(Default)]
pub struct QueryStatsStore {
    /// `internalQueryStatsRateLimit`.
    rate_limit: AtomicI32,
    window: Mutex<Window>,
    entries: Mutex<Entries>,
    /// The queries whose cursors are still open, by cursor id.
    open: Mutex<HashMap<i64, Open>>,
    open_count: AtomicUsize,
}

/// `RateLimiter::WindowBasedPolicy`: a one-second window, the previous
/// window's count weighted by how much of the current one is left.
#[derive(Default)]
struct Window {
    start: Option<Instant>,
    previous: u64,
    current: u64,
}

#[derive(Default)]
struct Entries {
    /// Key hashes in the order their entries were made.
    order: Vec<String>,
    by_hash: HashMap<String, Entry>,
}

struct Entry {
    key: Document,
    query_shape_hash: String,
    metrics: Metrics,
}

/// What an entry is filed under.
#[derive(Clone)]
struct Key {
    debug: Document,
    hash: String,
    query_shape_hash: String,
}

struct Open {
    key: Key,
    snapshot: Snapshot,
    /// A `getMore` of the cursor is running: its own counts are still to
    /// come when the cursor closes.
    busy: bool,
    closed: bool,
    /// A change stream: recorded at each operation.
    never_exhausts: bool,
}

/// What one execution of a query counted (`QueryStatsSnapshot`).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Snapshot {
    exec_micros: u64,
    first_response_micros: u64,
    docs_returned: u64,
    keys_examined: u64,
    docs_examined: u64,
    bytes_read: u64,
    read_micros: u64,
    working_millis: u64,
    /// -1 where the platform does not measure it.
    cpu_nanos: i64,
    delinquent_acquisitions: u64,
    total_delinquency_millis: u64,
    max_delinquency_millis: u64,
    interrupt_checks: u64,
    has_sort_stage: bool,
    used_disk: bool,
    from_multi_planner: bool,
    from_plan_cache: bool,
}

impl Snapshot {
    /// A `getMore`'s counts added to its cursor's (`AdditiveMetrics::add`):
    /// the plan's flags are the command's that opened it.
    fn add(&mut self, more: &Snapshot) {
        self.exec_micros += more.exec_micros;
        self.docs_returned += more.docs_returned;
        self.keys_examined += more.keys_examined;
        self.docs_examined += more.docs_examined;
        self.bytes_read += more.bytes_read;
        self.read_micros += more.read_micros;
        self.working_millis += more.working_millis;
        self.cpu_nanos = if self.cpu_nanos < 0 || more.cpu_nanos < 0 {
            -1
        } else {
            self.cpu_nanos + more.cpu_nanos
        };
        self.delinquent_acquisitions += more.delinquent_acquisitions;
        self.total_delinquency_millis += more.total_delinquency_millis;
        self.max_delinquency_millis = self.max_delinquency_millis.max(more.max_delinquency_millis);
        self.interrupt_checks += more.interrupt_checks;
        self.has_sort_stage |= more.has_sort_stage;
        self.used_disk |= more.used_disk;
    }
}

/// `AggregatedMetric`: sum, max, min and the sum of squares.
#[derive(Clone, Copy, Debug)]
struct Aggregated {
    sum: i64,
    max: i64,
    min: i64,
    sum_of_squares: i128,
}

impl Default for Aggregated {
    fn default() -> Self {
        Aggregated {
            sum: 0,
            max: i64::MIN,
            min: i64::MAX,
            sum_of_squares: 0,
        }
    }
}

impl Aggregated {
    fn aggregate(&mut self, v: i64) {
        self.sum = self.sum.saturating_add(v);
        self.max = self.max.max(v);
        self.min = self.min.min(v);
        self.sum_of_squares = self
            .sum_of_squares
            .saturating_add(i128::from(v) * i128::from(v));
    }

    fn unsigned(&mut self, v: u64) {
        self.aggregate(i64::try_from(v).unwrap_or(i64::MAX));
    }

    fn to_doc(self) -> Document {
        let squares = self
            .sum_of_squares
            .to_string()
            .parse::<Decimal128>()
            .map_or(Bson::Null, Bson::Decimal128);
        doc! {
            "sum": self.sum,
            "max": self.max,
            "min": self.min,
            "sumOfSquares": squares,
        }
    }
}

/// `AggregatedBool`.
#[derive(Clone, Copy, Default, Debug)]
struct Counted {
    yes: i64,
    no: i64,
}

impl Counted {
    fn aggregate(&mut self, v: bool) {
        if v {
            self.yes += 1;
        } else {
            self.no += 1;
        }
    }

    fn to_doc(self) -> Document {
        doc! { "true": self.yes, "false": self.no }
    }
}

#[derive(Default, Debug)]
struct Metrics {
    last_execution_micros: i64,
    exec_count: i64,
    total_exec_micros: Aggregated,
    first_response_exec_micros: Aggregated,
    docs_returned: Aggregated,
    keys_examined: Aggregated,
    docs_examined: Aggregated,
    bytes_read: Aggregated,
    read_time_micros: Aggregated,
    working_time_millis: Aggregated,
    cpu_nanos: Aggregated,
    delinquent_acquisitions: Aggregated,
    total_acquisition_delinquency_millis: Aggregated,
    max_acquisition_delinquency_millis: Aggregated,
    num_interrupt_checks_per_sec: Aggregated,
    has_sort_stage: Counted,
    used_disk: Counted,
    from_multi_planner: Counted,
    from_plan_cache: Counted,
    first_seen: Option<DateTime>,
    latest_seen: Option<DateTime>,
}

impl Metrics {
    /// `updateStatistics`.
    fn update(&mut self, s: &Snapshot, now: DateTime) {
        self.first_seen.get_or_insert(now);
        self.latest_seen = Some(now);
        self.last_execution_micros = i64::try_from(s.exec_micros).unwrap_or(i64::MAX);
        self.exec_count += 1;
        self.total_exec_micros.unsigned(s.exec_micros);
        self.first_response_exec_micros
            .unsigned(s.first_response_micros);
        self.docs_returned.unsigned(s.docs_returned);
        self.keys_examined.unsigned(s.keys_examined);
        self.docs_examined.unsigned(s.docs_examined);
        self.bytes_read.unsigned(s.bytes_read);
        self.read_time_micros.unsigned(s.read_micros);
        self.working_time_millis.unsigned(s.working_millis);
        self.cpu_nanos.aggregate(s.cpu_nanos);
        self.delinquent_acquisitions
            .unsigned(s.delinquent_acquisitions);
        self.total_acquisition_delinquency_millis
            .unsigned(s.total_delinquency_millis);
        self.max_acquisition_delinquency_millis
            .unsigned(s.max_delinquency_millis);
        // Interrupt checks per whole second of working time; none under a
        // second (8.2.11's `updateStatistics`).
        let seconds = s.working_millis / 1000;
        let rate = if seconds == 0 {
            0
        } else {
            (s.interrupt_checks as f64 / seconds as f64) as u64
        };
        self.num_interrupt_checks_per_sec.unsigned(rate);
        self.has_sort_stage.aggregate(s.has_sort_stage);
        self.used_disk.aggregate(s.used_disk);
        self.from_multi_planner.aggregate(s.from_multi_planner);
        self.from_plan_cache.aggregate(s.from_plan_cache);
    }

    /// `QueryStatsEntry::toBSON`, in mongod 8.2.11's order.
    fn to_doc(&self) -> Document {
        let mut out = doc! {
            "lastExecutionMicros": self.last_execution_micros,
            "execCount": self.exec_count,
            "totalExecMicros": self.total_exec_micros.to_doc(),
            "firstResponseExecMicros": self.first_response_exec_micros.to_doc(),
            "docsReturned": self.docs_returned.to_doc(),
            "keysExamined": self.keys_examined.to_doc(),
            "docsExamined": self.docs_examined.to_doc(),
            "bytesRead": self.bytes_read.to_doc(),
            "readTimeMicros": self.read_time_micros.to_doc(),
            "workingTimeMillis": self.working_time_millis.to_doc(),
        };
        // Only where the platform measures it (`appendToIfNonNegative`).
        if self.cpu_nanos.sum >= 0 {
            out.insert("cpuNanos", self.cpu_nanos.to_doc());
        }
        out.insert(
            "delinquentAcquisitions",
            self.delinquent_acquisitions.to_doc(),
        );
        out.insert(
            "totalAcquisitionDelinquencyMillis",
            self.total_acquisition_delinquency_millis.to_doc(),
        );
        out.insert(
            "maxAcquisitionDelinquencyMillis",
            self.max_acquisition_delinquency_millis.to_doc(),
        );
        out.insert(
            "numInterruptChecksPerSec",
            self.num_interrupt_checks_per_sec.to_doc(),
        );
        out.insert("hasSortStage", self.has_sort_stage.to_doc());
        out.insert("usedDisk", self.used_disk.to_doc());
        out.insert("fromMultiPlanner", self.from_multi_planner.to_doc());
        out.insert("fromPlanCache", self.from_plan_cache.to_doc());
        if let Some(t) = self.first_seen {
            out.insert("firstSeenTimestamp", t);
        }
        if let Some(t) = self.latest_seen {
            out.insert("latestSeenTimestamp", t);
        }
        out
    }
}

impl QueryStatsStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// `internalQueryStatsRateLimit` changed.
    pub fn set_rate_limit(&self, limit: i32) {
        self.rate_limit.store(limit, Ordering::Relaxed);
    }

    /// `shouldCollect`: off at 0, every query below 0, else what the
    /// sliding window lets through.
    fn should_collect(&self) -> bool {
        let limit = self.rate_limit.load(Ordering::Relaxed);
        if limit == 0 {
            return false;
        }
        if limit < 0 {
            return true;
        }
        let mut w = self.window.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let start = *w.start.get_or_insert(now);
        let period = Duration::from_secs(1);
        let start = if now.duration_since(start) > period {
            w.start = Some(now);
            w.previous = w.current;
            w.current = 0;
            now
        } else {
            start
        };
        let elapsed = now.duration_since(start).as_millis() as f64;
        let remaining = (1000.0 - elapsed) / 1000.0;
        let estimated = w.current as f64 + w.previous as f64 * remaining;
        if estimated < f64::from(limit) {
            w.current += 1;
            true
        } else {
            false
        }
    }

    fn write(&self, key: &Key, snapshot: &Snapshot) {
        let now = DateTime::now();
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let Entries { order, by_hash } = &mut *entries;
        let entry = by_hash.entry(key.hash.clone()).or_insert_with(|| {
            order.push(key.hash.clone());
            Entry {
                key: key.debug.clone(),
                query_shape_hash: key.query_shape_hash.clone(),
                metrics: Metrics::default(),
            }
        });
        entry.metrics.update(snapshot, now);
    }

    /// The entries, as `$queryStats` lists them.
    pub fn rows(&self) -> Vec<Document> {
        let as_of = DateTime::now();
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        entries
            .order
            .iter()
            .filter_map(|hash| {
                let e = entries.by_hash.get(hash)?;
                Some(doc! {
                    "key": e.key.clone(),
                    "keyHash": hash.clone(),
                    "queryShapeHash": e.query_shape_hash.clone(),
                    "metrics": e.metrics.to_doc(),
                    "asOf": as_of,
                })
            })
            .collect()
    }

    fn open(&self, cursor_id: i64, key: Key, snapshot: Snapshot, never_exhausts: bool) {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        open.insert(
            cursor_id,
            Open {
                key,
                snapshot,
                busy: false,
                closed: false,
                never_exhausts,
            },
        );
        self.open_count.store(open.len(), Ordering::Relaxed);
    }

    /// Cursor `cursor_id` closed. Recorded now, unless a `getMore` of it is
    /// running: that `getMore` records it, its own counts added.
    fn closed(&self, cursor_id: i64) {
        let done = {
            let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
            match open.get_mut(&cursor_id) {
                Some(o) if o.busy => {
                    o.closed = true;
                    None
                }
                Some(_) => {
                    let o = open.remove(&cursor_id);
                    self.open_count.store(open.len(), Ordering::Relaxed);
                    o
                }
                None => None,
            }
        };
        if let Some(o) = done {
            if !o.never_exhausts {
                self.write(&o.key, &o.snapshot);
            }
        }
    }

    /// A `getMore` of cursor `cursor_id` is about to run.
    fn get_more_begins(&self, cursor_id: i64) {
        if self.open_count.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(o) = open.get_mut(&cursor_id) {
            o.busy = true;
        }
    }

    /// A `getMore` of cursor `cursor_id` counted `more`.
    fn get_more_ends(&self, cursor_id: i64, more: &Snapshot) {
        if self.open_count.load(Ordering::Relaxed) == 0 {
            return;
        }
        let (write, key) = {
            let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
            let Some(o) = open.get_mut(&cursor_id) else {
                return;
            };
            o.busy = false;
            if o.never_exhausts {
                // A change stream records each operation on its own.
                let mut each = *more;
                each.from_multi_planner = o.snapshot.from_multi_planner;
                each.from_plan_cache = o.snapshot.from_plan_cache;
                let key = o.key.clone();
                if o.closed {
                    open.remove(&cursor_id);
                    self.open_count.store(open.len(), Ordering::Relaxed);
                }
                (Some(each), Some(key))
            } else {
                o.snapshot.add(more);
                if o.closed {
                    let o = open.remove(&cursor_id).expect("present");
                    self.open_count.store(open.len(), Ordering::Relaxed);
                    (Some(o.snapshot), Some(o.key))
                } else {
                    (None, None)
                }
            }
        };
        if let (Some(snapshot), Some(key)) = (write, key) {
            self.write(&key, &snapshot);
        }
    }
}

thread_local! {
    /// The store of the server whose `aggregate` runs on this thread, for
    /// its `$queryStats` stage.
    static CURRENT: std::cell::RefCell<Option<Arc<QueryStatsStore>>> =
        const { std::cell::RefCell::new(None) };
}

/// The `aggregate` about to run is the server's whose store this is.
pub(crate) fn set_current(store: Option<Arc<QueryStatsStore>>) {
    CURRENT.with(|c| *c.borrow_mut() = store);
}

/// `$queryStats`' rows (its argument checked already). With
/// `transformIdentifiers` mongod prints each identifier as its HMAC, which
/// this store cannot: it refuses once it has entries to print (RM-215's
/// remainder in the backlog).
pub(crate) fn stage_rows(spec: Option<&Bson>) -> Result<Vec<Document>, crate::CommandError> {
    let rows = CURRENT.with(|c| c.borrow().as_ref().map(|s| s.rows()).unwrap_or_default());
    let transformed = spec
        .and_then(Bson::as_document)
        .is_some_and(|s| s.contains_key("transformIdentifiers"));
    if transformed && !rows.is_empty() {
        return Err(crate::CommandError::new(
            238,
            "NotImplemented",
            "$queryStats with transformIdentifiers is not supported by this server",
        ));
    }
    Ok(rows)
}

/// The store of the server `ctx` runs on.
fn store(ctx: &CommandContext) -> Option<Arc<QueryStatsStore>> {
    ctx.server_params.as_ref().map(|p| p.query_stats().clone())
}

/// Before a command runs: a `getMore` holds its cursor's pending record.
pub fn before_operation(request: &Document, ctx: &CommandContext) {
    if request.keys().next().map(String::as_str) != Some("getMore") {
        return;
    }
    if let (Some(store), Some(id)) = (store(ctx), cursor_of(request)) {
        store.get_more_begins(id);
    }
}

fn cursor_of(request: &Document) -> Option<i64> {
    match request.get("getMore")? {
        Bson::Int64(n) => Some(*n),
        Bson::Int32(n) => Some(i64::from(*n)),
        _ => None,
    }
}

/// After a command ran for `worked` (its awaited time left out): record
/// it, or hold it until its cursor closes.
pub fn after_operation(
    request: &Document,
    reply: &Document,
    ctx: &CommandContext,
    worked: Duration,
) {
    let Some(name) = request.keys().next().map(String::as_str) else {
        return;
    };
    if !matches!(
        name,
        "find" | "aggregate" | "count" | "distinct" | "getMore"
    ) {
        return;
    }
    let Some(store) = store(ctx) else {
        return;
    };
    if name == "getMore" {
        if let Some(id) = cursor_of(request) {
            if store.open_count.load(Ordering::Relaxed) > 0 {
                let more = snapshot(name, reply, ctx, worked);
                store.get_more_ends(id, &more);
            }
        }
        return;
    }
    let ok = reply.get_f64("ok").unwrap_or(0.0) == 1.0;
    // An `aggregate` that explains is recorded with its verbosity, which
    // this store does not shape (RM-215's remainder).
    if !ok || request.contains_key("explain") || store.rate_limit.load(Ordering::Relaxed) == 0 {
        return;
    }
    let Some(shape) = crate::query_shape::stats_shape(&ctx.db_name, request) else {
        return;
    };
    if !store.should_collect() {
        return;
    }
    let key = key(request, ctx, &shape);
    let snapshot = snapshot(name, reply, ctx, worked);
    let cursor_id = reply
        .get_document("cursor")
        .ok()
        .and_then(|c| c.get_i64("id").ok())
        .unwrap_or(0);
    if cursor_id == 0 {
        store.write(&key, &snapshot);
        return;
    }
    let never_exhausts = changes(request);
    if never_exhausts {
        store.write(&key, &snapshot);
    }
    store.open(cursor_id, key, snapshot, never_exhausts);
    let on_close = {
        let store = store.clone();
        crate::cursors::OnClose::new(move || store.closed(cursor_id))
    };
    let attached = match ctx.cursors.as_ref() {
        Some(c) => c.on_close(cursor_id, on_close),
        None => Err(on_close),
    };
    if let Err(on_close) = attached {
        // The cursor closed already.
        on_close.disarm();
        store.closed(cursor_id);
    }
}

/// A change stream's `aggregate`.
fn changes(request: &Document) -> bool {
    request
        .get_array("pipeline")
        .ok()
        .and_then(|p| p.first())
        .and_then(Bson::as_document)
        .is_some_and(|s| s.contains_key("$changeStream"))
}

/// What the operation counted (`captureMetrics`).
fn snapshot(name: &str, reply: &Document, ctx: &CommandContext, worked: Duration) -> Snapshot {
    let exec = ctx.exec_stats.as_ref();
    let micros = u64::try_from(worked.as_micros()).unwrap_or(u64::MAX);
    let returned = match name {
        "count" => 1,
        "distinct" => reply.get_array("values").map_or(0, Vec::len),
        _ => match &ctx.pending_batch {
            Some(pending) => pending.batch.len(),
            None => reply
                .get_document("cursor")
                .ok()
                .and_then(|c| {
                    c.get_array("firstBatch")
                        .or_else(|_| c.get_array("nextBatch"))
                        .ok()
                })
                .map_or(0, Vec::len),
        },
    };
    let spilled = secantus_core::memlimit::report();
    let counts = ctx.op_counts.as_ref();
    let admission = &ctx.execution_admission;
    let planned = ctx.planned.unwrap_or_default();
    let unsigned = |n: i64| u64::try_from(n).unwrap_or(0);
    let delinquent = |n: i32| u64::try_from(n).unwrap_or(0);
    Snapshot {
        exec_micros: micros,
        first_response_micros: micros,
        docs_returned: returned as u64,
        keys_examined: exec.map_or(0, |e| unsigned(e.keys_examined)),
        docs_examined: exec.map_or(0, |e| unsigned(e.docs_examined)),
        bytes_read: counts.map_or(0, |c| unsigned(c.bytes_read)),
        read_micros: counts.map_or(0, |c| unsigned(c.read_micros)),
        working_millis: u64::try_from(worked.as_millis()).unwrap_or(u64::MAX),
        cpu_nanos: ctx.cpu_nanos.unwrap_or(-1),
        delinquent_acquisitions: delinquent(admission.delinquent_acquisitions()),
        total_delinquency_millis: unsigned(admission.total_acquisition_delinquency_millis()),
        max_delinquency_millis: unsigned(admission.max_acquisition_delinquency_millis()),
        interrupt_checks: unsigned(secantus_core::deadline::interrupt_checks()),
        has_sort_stage: exec.is_some_and(|e| e.has_sort_stage) || spilled.has_sort_stage,
        used_disk: spilled.used_disk,
        from_multi_planner: planned.from_multi_planner,
        from_plan_cache: planned.from_plan_cache,
    }
}

/// The entry's key (`Key::toBson`): the shape, then the universal
/// components in mongod's order, then the command's own.
fn key(request: &Document, ctx: &CommandContext, shape: &StatsShape) -> Key {
    let representative = key_doc(request, ctx, &shape.representative, false);
    let debug = key_doc(request, ctx, &shape.debug, true);
    let bytes = crate::bsondoc::to_vec(&representative).unwrap_or_default();
    let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&bytes));
    Key {
        debug,
        hash,
        query_shape_hash: shape.hash.clone(),
    }
}

fn literal(v: &Bson, debug: bool) -> Bson {
    crate::query_shape::literal(v, debug).unwrap_or(Bson::Null)
}

fn key_doc(request: &Document, ctx: &CommandContext, shape: &Document, debug: bool) -> Document {
    let name = request.keys().next().map(String::as_str).unwrap_or("");
    let mut key = doc! { "queryShape": shape.clone() };
    // `UniversalKeyComponents::appendTo`.
    if let Some(comment) = request.get("comment") {
        key.insert("comment", literal(comment, debug));
    }
    if let Ok(rc) = request.get_document("readConcern") {
        key.insert("readConcern", read_concern(rc, debug));
    }
    for field in ["apiVersion", "apiStrict", "apiDeprecationErrors"] {
        if let Some(v) = request.get(field) {
            key.insert(field, v.clone());
        }
    }
    if let Ok(rp) = request.get_document("$readPreference") {
        key.insert("$readPreference", read_preference(rp));
    }
    if let Ok(wc) = request.get_document("writeConcern") {
        key.insert("writeConcern", wc.clone());
    }
    if let Some(client) = client(ctx) {
        key.insert("client", client);
    }
    let shape_ns = shape.get_document("cmdNs").ok();
    let db = shape_ns
        .and_then(|n| n.get_str("db").ok())
        .unwrap_or(&ctx.db_name);
    let coll = shape_ns.and_then(|n| n.get_str("coll").ok()).unwrap_or("");
    key.insert("collectionType", collection_type(request, ctx, db, coll));
    if let Some(hint) = hint(request) {
        key.insert("hint", hint);
    }
    if request.contains_key("maxTimeMS") {
        key.insert("maxTimeMS", literal(&Bson::Int64(0), debug));
    }
    match name {
        // `FindCmdComponents::appendTo`.
        "find" => {
            for field in ["allowPartialResults", "noCursorTimeout"] {
                if let Some(v) = request.get(field).and_then(Bson::as_bool) {
                    key.insert(field, v);
                }
            }
            if request.contains_key("batchSize") {
                key.insert("batchSize", literal(&Bson::Int64(0), debug));
            }
        }
        // `AggCmdComponents::appendTo`.
        "aggregate" => {
            let others = involved(request, db);
            if !others.is_empty() {
                key.insert(
                    "otherNss",
                    others
                        .into_iter()
                        .map(|(db, coll)| Bson::Document(doc! { "db": db, "coll": coll }))
                        .collect::<Vec<_>>(),
                );
            }
            if let Some(v) = request
                .get("bypassDocumentValidation")
                .and_then(Bson::as_bool)
            {
                key.insert("bypassDocumentValidation", v);
            }
            // The cursor's batch size: mongod fills in its default.
            key.insert(
                "cursor",
                doc! { "batchSize": literal(&Bson::Int64(0), debug) },
            );
        }
        _ => {}
    }
    key
}

/// `shapifyReadConcern`: a cluster time as a literal.
fn read_concern(rc: &Document, debug: bool) -> Document {
    if !rc.contains_key("afterClusterTime") && !rc.contains_key("atClusterTime") {
        return rc.clone();
    }
    let mut out = Document::new();
    if let Some(level) = rc.get("level") {
        out.insert("level", level.clone());
    }
    for field in ["afterClusterTime", "atClusterTime"] {
        if let Some(t) = rc.get(field) {
            // The stored key keeps the time as it was given.
            out.insert(field, if debug { literal(t, true) } else { t.clone() });
        }
    }
    out
}

/// `shapifyReadPreference`: its tags in BSON order.
fn read_preference(rp: &Document) -> Document {
    rp.iter()
        .map(|(k, v)| match (k.as_str(), v) {
            ("tags", Bson::Array(tags)) => {
                let mut tags = tags.clone();
                tags.sort_by(|a, b| {
                    let bytes = |x: &Bson| {
                        x.as_document()
                            .and_then(|d| crate::bsondoc::to_vec(d).ok())
                            .unwrap_or_default()
                    };
                    bytes(a).cmp(&bytes(b))
                });
                (k.clone(), Bson::Array(tags))
            }
            _ => (k.clone(), v.clone()),
        })
        .collect()
}

/// The client's metadata from its handshake, less what a `mongos` adds.
fn client(ctx: &CommandContext) -> Option<Document> {
    let auth = ctx.conn_auth.as_ref()?.lock().ok()?;
    let mut client = auth.client_metadata.clone()?;
    client.remove("mongos");
    Some(client)
}

/// `extractHintShape`: an index's name or key pattern as given.
fn hint(request: &Document) -> Option<Document> {
    match request.get("hint")? {
        Bson::String(name) => Some(doc! { "$hint": name.clone() }),
        Bson::Document(d) if !d.is_empty() => Some(d.clone()),
        _ => None,
    }
}

/// `query_shape::CollectionType`.
fn collection_type(request: &Document, ctx: &CommandContext, db: &str, coll: &str) -> &'static str {
    if coll.starts_with("$cmd") {
        return "virtual";
    }
    if changes(request) {
        return "changeStream";
    }
    let Some(storage) = ctx.storage.as_ref() else {
        return "collection";
    };
    if crate::views::is_view(storage.as_ref(), db, coll) {
        return "view";
    }
    if !storage.collection_exists(db, coll).unwrap_or(true) {
        return "nonExistent";
    }
    if storage
        .get_collection_options(db, coll)
        .is_ok_and(|o| o.contains_key("timeseries"))
    {
        return "timeseries";
    }
    "collection"
}

/// The namespaces a pipeline reads besides its own
/// (`LiteParsedPipeline::getInvolvedNamespaces`), in the order it names
/// them.
fn involved(request: &Document, db: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(pipeline) = request.get_array("pipeline") {
        walk(pipeline, db, &mut out);
    }
    out
}

fn walk(pipeline: &[Bson], db: &str, out: &mut Vec<(String, String)>) {
    let add = |ns: (String, String), out: &mut Vec<(String, String)>| {
        if !out.contains(&ns) {
            out.push(ns);
        }
    };
    for stage in pipeline.iter().filter_map(Bson::as_document) {
        let Some((name, spec)) = stage.iter().next() else {
            continue;
        };
        let spec = spec.as_document();
        match name.as_str() {
            "$lookup" | "$graphLookup" => {
                if let Some(s) = spec {
                    match s.get("from") {
                        Some(Bson::String(c)) => add((db.to_string(), c.clone()), out),
                        Some(Bson::Document(d)) => {
                            if let (Ok(d_db), Ok(c)) = (d.get_str("db"), d.get_str("coll")) {
                                add((d_db.to_string(), c.to_string()), out);
                            }
                        }
                        _ => {}
                    }
                    if let Ok(p) = s.get_array("pipeline") {
                        walk(p, db, out);
                    }
                }
            }
            "$unionWith" => match stage.get("$unionWith") {
                Some(Bson::String(c)) => add((db.to_string(), c.clone()), out),
                Some(Bson::Document(s)) => {
                    if let Ok(c) = s.get_str("coll") {
                        add((db.to_string(), c.to_string()), out);
                    }
                    if let Ok(p) = s.get_array("pipeline") {
                        walk(p, db, out);
                    }
                }
                _ => {}
            },
            // Where it writes (`$out`, `$merge`'s `into`).
            "$out" | "$merge" => {
                let target = match stage.get(name.as_str()) {
                    Some(Bson::Document(s)) if name == "$merge" => s.get("into"),
                    other => other,
                };
                match target {
                    Some(Bson::String(c)) => add((db.to_string(), c.clone()), out),
                    Some(Bson::Document(d)) => {
                        if let Ok(c) = d.get_str("coll") {
                            let d_db = d.get_str("db").unwrap_or(db);
                            add((d_db.to_string(), c.to_string()), out);
                        }
                    }
                    _ => {}
                }
            }
            "$facet" => {
                if let Some(s) = spec {
                    for p in s.values().filter_map(Bson::as_array) {
                        walk(p, db, out);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(micros: u64, returned: u64) -> Snapshot {
        Snapshot {
            exec_micros: micros,
            first_response_micros: micros,
            docs_returned: returned,
            cpu_nanos: -1,
            ..Default::default()
        }
    }

    fn key(hash: &str) -> Key {
        Key {
            debug: doc! { "queryShape": { "command": "find" } },
            hash: hash.into(),
            query_shape_hash: "H".into(),
        }
    }

    #[test]
    fn nothing_is_collected_at_the_default_rate_limit() {
        let store = QueryStatsStore::new();
        assert!(!store.should_collect());
        store.set_rate_limit(-1);
        assert!(store.should_collect());
    }

    #[test]
    fn a_rate_limit_lets_that_many_through_a_second() {
        let store = QueryStatsStore::new();
        store.set_rate_limit(3);
        let let_through = (0..10).filter(|_| store.should_collect()).count();
        assert_eq!(let_through, 3);
    }

    #[test]
    fn an_entry_aggregates_its_executions() {
        let store = QueryStatsStore::new();
        store.write(&key("k"), &snap(10, 2));
        store.write(&key("k"), &snap(30, 4));
        let rows = store.rows();
        assert_eq!(rows.len(), 1);
        let m = rows[0].get_document("metrics").unwrap();
        assert_eq!(m.get_i64("execCount").unwrap(), 2);
        assert_eq!(m.get_i64("lastExecutionMicros").unwrap(), 30);
        let total = m.get_document("totalExecMicros").unwrap();
        assert_eq!(total.get_i64("sum").unwrap(), 40);
        assert_eq!(total.get_i64("max").unwrap(), 30);
        assert_eq!(total.get_i64("min").unwrap(), 10);
        assert_eq!(
            total.get("sumOfSquares"),
            Some(&Bson::Decimal128("1000".parse().unwrap()))
        );
        // No CPU clock: no `cpuNanos`.
        assert!(!m.contains_key("cpuNanos"));
        let keys: Vec<&str> = rows[0].keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["key", "keyHash", "queryShapeHash", "metrics", "asOf"]
        );
    }

    #[test]
    fn a_cursor_is_recorded_when_it_closes_with_its_get_mores() {
        let store = QueryStatsStore::new();
        store.open(7, key("k"), snap(100, 2), false);
        store.get_more_begins(7);
        store.get_more_ends(7, &snap(50, 2));
        assert!(store.rows().is_empty());
        // The last getMore exhausts it: closed while that getMore runs.
        store.get_more_begins(7);
        store.closed(7);
        assert!(store.rows().is_empty());
        store.get_more_ends(7, &snap(25, 1));
        let rows = store.rows();
        let m = rows[0].get_document("metrics").unwrap();
        assert_eq!(m.get_i64("execCount").unwrap(), 1);
        assert_eq!(
            m.get_document("docsReturned")
                .unwrap()
                .get_i64("sum")
                .unwrap(),
            5
        );
        assert_eq!(
            m.get_document("totalExecMicros")
                .unwrap()
                .get_i64("sum")
                .unwrap(),
            175
        );
        assert_eq!(
            m.get_document("firstResponseExecMicros")
                .unwrap()
                .get_i64("sum")
                .unwrap(),
            100
        );
        assert_eq!(store.open_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_killed_cursor_is_recorded_with_what_it_returned() {
        let store = QueryStatsStore::new();
        store.open(9, key("k"), snap(100, 2), false);
        store.closed(9);
        let rows = store.rows();
        let m = rows[0].get_document("metrics").unwrap();
        assert_eq!(
            m.get_document("docsReturned")
                .unwrap()
                .get_i64("sum")
                .unwrap(),
            2
        );
    }
}
