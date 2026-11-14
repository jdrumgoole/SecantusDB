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
//! a `getMore` fails on is, as it closes. An `aggregate` that explains is
//! recorded with its verbosity in its key (RM-532).
//!
//! The store is mongod's partitioned LRU cache (RM-532): its budget is
//! `internalQueryStatsCacheSize`, 0 bytes turns query stats off, and
//! `internalQueryStatsSampleRate` above 0 samples in place of the rate
//! limit. `$queryStats` with `transformIdentifiers` shapes each key again
//! with every identifier as its HMAC-SHA-256 (`query_shape::with_hmac`).
//! `serverStatus` reports the store under `metrics.queryStats`.

use crate::util::ReplyOk;
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use bson::{doc, Bson, DateTime, Decimal128, Document};
use sha2::{Digest, Sha256};

use crate::query_shape::StatsShape;
use crate::CommandContext;

/// The store, one per server: mongod's `QueryStatsStore`, a
/// `PartitionedCache` of least-recently-used maps. Its budget,
/// `internalQueryStatsCacheSize` (1% of the machine's memory, capped at
/// 1 GB and at 25%), is split evenly across the partitions; a partition
/// over its share evicts its least recently used entries. The number of
/// partitions is fixed when the server starts: one per 16 MB of the
/// budget, and at least one per logical core.
pub struct QueryStatsStore {
    /// `internalQueryStatsRateLimit`.
    rate_limit: AtomicI32,
    /// `internalQueryStatsSampleRate`, in thousandths rounded up
    /// (`roundSampleRateToPerThousand`); above 0 it decides instead of the
    /// rate limit.
    sample_rate: AtomicI32,
    window: Mutex<Window>,
    partitions: Vec<Mutex<Partition>>,
    /// The budget in bytes, capped; 0 turns query stats off.
    max_size: AtomicUsize,
    /// `serverStatus`' `metrics.queryStats` counters.
    max_size_metric: AtomicU64,
    evicted: AtomicU64,
    write_errors: AtomicU64,
    rate_limited: AtomicU64,
    hmac_errors: AtomicU64,
    /// The queries whose cursors are still open, by cursor id.
    open: Mutex<HashMap<i64, Open>>,
    open_count: AtomicUsize,
}

impl Default for QueryStatsStore {
    fn default() -> Self {
        let host = crate::hostinfo::Host::read();
        let memory = memory_bytes(&host);
        let size = cap_size(convert(DEFAULT_CACHE_SIZE, memory), memory);
        let cores = usize::try_from(host.logical_cores.max(1)).unwrap_or(1);
        Self::with_size(size, cores)
    }
}

/// `internalQueryStatsCacheSize`'s default.
pub(crate) const DEFAULT_CACHE_SIZE: &str = "1%";

/// The machine's memory (`ProcessInfo::getMemSizeBytes`).
fn memory_bytes(host: &crate::hostinfo::Host) -> u64 {
    // In bytes, not whole MiB: on Linux `MemTotal` is kB, and 1% of it
    // rounded to MiB first is a few hundred bytes short (RM-657). A
    // platform that cannot say is taken as 1 GB.
    host.mem_size_bytes.unwrap_or(1024 * 1024 * 1024)
}

/// A size `internalQueryStatsCacheSize` names
/// (`memory_util::MemorySize::parse`, `(?i)^\s*(\d+\.?\d*)\s*(B|KB|MB|GB|%)?\s*$`):
/// a number, then optionally a unit -- `B`, `KB`, `MB`, `GB` or `%`, in
/// any case -- with spaces around either.
pub(crate) fn parse_size(text: &str) -> Option<(f64, char)> {
    let t = text.trim_matches(|c: char| c.is_ascii_whitespace());
    let whole = t.bytes().take_while(u8::is_ascii_digit).count();
    if whole == 0 {
        return None;
    }
    let mut end = whole;
    if t.as_bytes().get(end) == Some(&b'.') {
        end += 1 + t[end + 1..].bytes().take_while(u8::is_ascii_digit).count();
    }
    let (number, unit) = t.split_at(end);
    let unit = match unit
        .trim_start_matches(|c: char| c.is_ascii_whitespace())
        .to_ascii_uppercase()
        .as_str()
    {
        "" | "B" => 'B',
        "KB" => 'K',
        "MB" => 'M',
        "GB" => 'G',
        "%" => '%',
        _ => return None,
    };
    Some((number.parse().ok()?, unit))
}

/// `convertToSizeInBytes`: a size, as a whole number of bytes.
fn convert(text: &str, memory: u64) -> u64 {
    let Some((n, unit)) = parse_size(text) else {
        return 0;
    };
    let bytes = match unit {
        '%' => n * (memory as f64 / 100.0),
        'K' => n * 1024.0,
        'M' => n * 1024.0 * 1024.0,
        'G' => n * 1024.0 * 1024.0 * 1024.0,
        _ => n,
    };
    bytes as u64
}

/// `capQueryStatsStoreSize`: at most 1 GB, and at most 25% of memory.
fn cap_size(requested: u64, memory: u64) -> u64 {
    let quarter = (25.0 * (memory as f64 / 100.0)) as u64;
    requested.min(1024 * 1024 * 1024).min(quarter)
}

/// The bytes one entry is charged beyond its key's representative BSON:
/// what mongod 8.2.11's `QueryStatsStoreEntryBudgetor` adds for the
/// entry's own structures (`sizeof` of the entry, its hash and its key's
/// components), measured per command from its
/// `queryStatsStoreSizeEstimateBytes` (2026-11-14).
fn entry_overhead(command: &str) -> usize {
    match command {
        "find" => 1191,
        "aggregate" => 1096,
        "distinct" => 1005,
        _ => 990,
    }
}

/// `RateLimiter::WindowBasedPolicy`: a one-second window, the previous
/// window's count weighted by how much of the current one is left.
#[derive(Default)]
struct Window {
    start: Option<Instant>,
    previous: u64,
    current: u64,
}

/// One partition (`LRUKeyValue`): its entries by key hash, the least
/// recently used first in `lru`.
#[derive(Default)]
struct Partition {
    entries: HashMap<String, Slot>,
    lru: BTreeMap<u64, String>,
    tick: u64,
    /// The bytes its entries are charged.
    size: usize,
}

struct Slot {
    entry: Entry,
    tick: u64,
}

impl Partition {
    /// The entry under `hash`, made the most recently used (`get`).
    fn get(&mut self, hash: &str) -> Option<&mut Entry> {
        let slot = self.entries.get_mut(hash)?;
        self.lru.remove(&slot.tick);
        self.tick += 1;
        slot.tick = self.tick;
        self.lru.insert(self.tick, hash.to_string());
        Some(&mut slot.entry)
    }

    /// Add `entry`, then evict down to `budget` (`add`); how many went.
    fn add(&mut self, hash: &str, entry: Entry, budget: usize) -> u64 {
        if let Some(old) = self.entries.remove(hash) {
            self.lru.remove(&old.tick);
            self.size -= old.entry.size;
        }
        self.tick += 1;
        self.size += entry.size;
        self.lru.insert(self.tick, hash.to_string());
        self.entries.insert(
            hash.to_string(),
            Slot {
                entry,
                tick: self.tick,
            },
        );
        self.evict(budget)
    }

    /// Evict the least recently used entries until the partition is within
    /// `budget` (`evict`); how many went.
    fn evict(&mut self, budget: usize) -> u64 {
        let mut evicted = 0;
        while self.size > budget {
            let Some((_, hash)) = self.lru.pop_first() else {
                break;
            };
            if let Some(slot) = self.entries.remove(&hash) {
                self.size -= slot.entry.size;
            }
            evicted += 1;
        }
        evicted
    }
}

struct Entry {
    key: Arc<KeySource>,
    query_shape_hash: String,
    /// What the entry is charged against its partition's share.
    size: usize,
    metrics: Metrics,
}

/// What an entry is filed under.
#[derive(Clone)]
struct Key {
    source: Arc<KeySource>,
    hash: String,
    query_shape_hash: String,
    size: usize,
}

/// What a key is printed from: its debug form, and the command it was made
/// from, which `transformIdentifiers` shapes again -- mongod re-serializes
/// the key it parsed.
struct KeySource {
    debug: Document,
    db: String,
    request: Document,
    parts: Parts,
}

/// The key's components that come from the server, not the command.
struct Parts {
    client: Option<Document>,
    collection_type: &'static str,
    /// An `aggregate` that explains: its verbosity.
    explain: Option<String>,
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

    fn with_size(size: u64, cores: usize) -> Self {
        // `ceil(size / 16 MB)`, and no fewer than the logical cores.
        let wanted = usize::try_from(size.div_ceil(16 * 1024 * 1024)).unwrap_or(usize::MAX);
        let partitions = wanted.max(cores).max(1);
        QueryStatsStore {
            rate_limit: AtomicI32::new(0),
            sample_rate: AtomicI32::new(0),
            window: Mutex::default(),
            partitions: (0..partitions).map(|_| Mutex::default()).collect(),
            max_size: AtomicUsize::new(usize::try_from(size).unwrap_or(usize::MAX)),
            max_size_metric: AtomicU64::new(size),
            evicted: AtomicU64::new(0),
            write_errors: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            hmac_errors: AtomicU64::new(0),
            open: Mutex::default(),
            open_count: AtomicUsize::new(0),
        }
    }

    /// `internalQueryStatsRateLimit` changed.
    pub fn set_rate_limit(&self, limit: i32) {
        self.rate_limit.store(limit, Ordering::Relaxed);
    }

    /// `internalQueryStatsSampleRate` changed (`configureRateLimiter`).
    pub fn set_sample_rate(&self, rate: f64) {
        let per_thousand = (rate * 1000.0).ceil() as i32;
        self.sample_rate.store(per_thousand, Ordering::Relaxed);
    }

    /// `internalQueryStatsCacheSize` changed (`updateCacheSize`): the
    /// budget becomes the new size, capped, and each partition evicts down
    /// to its share. The number of partitions stays as the server started.
    pub fn set_cache_size(&self, text: &str) {
        let memory = memory_bytes(&crate::hostinfo::Host::read());
        let requested = convert(text, memory);
        let capped = usize::try_from(cap_size(requested, memory)).unwrap_or(usize::MAX);
        self.max_size.store(capped, Ordering::Relaxed);
        self.max_size_metric.store(requested, Ordering::Relaxed);
        let budget = self.budget();
        let evicted: u64 = self
            .partitions
            .iter()
            .map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).evict(budget))
            .sum();
        self.evicted.fetch_add(evicted, Ordering::Relaxed);
    }

    /// `isQueryStatsEnabled`: a budget above 0 bytes.
    pub(crate) fn enabled(&self) -> bool {
        self.max_size.load(Ordering::Relaxed) > 0
    }

    /// Each partition's share of the budget.
    fn budget(&self) -> usize {
        self.max_size.load(Ordering::Relaxed) / self.partitions.len()
    }

    /// `serverStatus`' `metrics.queryStats`.
    pub fn server_status(&self) -> Document {
        let (mut entries, mut bytes) = (0usize, 0usize);
        for p in &self.partitions {
            let p = p.lock().unwrap_or_else(|e| e.into_inner());
            entries += p.entries.len();
            bytes += p.size;
        }
        let n = |v: u64| Bson::Int64(i64::try_from(v).unwrap_or(i64::MAX));
        doc! {
            "maxSizeBytes": n(self.max_size_metric.load(Ordering::Relaxed)),
            "numEntries": n(entries as u64),
            "numEvicted": n(self.evicted.load(Ordering::Relaxed)),
            "numHmacApplicationErrors": n(self.hmac_errors.load(Ordering::Relaxed)),
            "numPartitions": n(self.partitions.len() as u64),
            "numQueryStatsStoreWriteErrors": n(self.write_errors.load(Ordering::Relaxed)),
            "numRateLimitedRequests": n(self.rate_limited.load(Ordering::Relaxed)),
            "queryStatsStoreSizeEstimateBytes": n(bytes as u64),
        }
    }

    /// `shouldCollect`: by the sample rate when it is above 0, else by the
    /// rate limit -- none at 0, every query below 0, else what the sliding
    /// window lets through. A query either turns away is counted
    /// (`numRateLimitedRequests`).
    fn should_collect(&self) -> bool {
        let sample = self.sample_rate.load(Ordering::Relaxed);
        let admitted = if sample > 0 {
            rand::random_range(0..1000) < sample
        } else {
            let limit = self.rate_limit.load(Ordering::Relaxed);
            if limit == 0 {
                return false;
            }
            limit < 0 || self.window_admits(limit)
        };
        if !admitted {
            self.rate_limited.fetch_add(1, Ordering::Relaxed);
        }
        admitted
    }

    fn window_admits(&self, limit: i32) -> bool {
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

    /// The partition an entry lives in (`QueryStatsPartitioner`): its key's
    /// hash modulo the number of partitions.
    fn partition(&self, hash: &str) -> &Mutex<Partition> {
        let digest = Sha256::digest(hash.as_bytes());
        let n = u64::from_le_bytes(digest[..8].try_into().expect("eight bytes"));
        let at = usize::try_from(n % self.partitions.len() as u64).unwrap_or(0);
        &self.partitions[at]
    }

    /// `writeQueryStats`: add to the key's entry, or make one.
    fn write(&self, key: &Key, snapshot: &Snapshot) {
        // Turned off since the query began.
        if !self.enabled() {
            return;
        }
        let now = DateTime::now();
        let budget = self.budget();
        let mut p = self
            .partition(&key.hash)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = p.get(&key.hash) {
            entry.metrics.update(snapshot, now);
            return;
        }
        let entry = Entry {
            key: key.source.clone(),
            query_shape_hash: key.query_shape_hash.clone(),
            size: key.size,
            metrics: Metrics::default(),
        };
        let evicted = p.add(&key.hash, entry, budget);
        self.evicted.fetch_add(evicted, Ordering::Relaxed);
        match p.get(&key.hash) {
            Some(entry) => entry.metrics.update(snapshot, now),
            // Larger than its partition's share: evicted as it went in.
            None => {
                self.write_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The entries, as `$queryStats` lists them: partition by partition,
    /// each most recently used first, as of when it was copied. With an
    /// HMAC key each identifier is printed as its HMAC; an entry that
    /// cannot be is left out and counted (`numHmacApplicationErrors`).
    pub fn rows(&self, hmac_key: Option<&[u8]>) -> Vec<Document> {
        let mut out = Vec::new();
        for p in &self.partitions {
            let copied: Vec<(String, Arc<KeySource>, String, Document)> = {
                let p = p.lock().unwrap_or_else(|e| e.into_inner());
                p.lru
                    .values()
                    .rev()
                    .filter_map(|hash| {
                        let e = &p.entries.get(hash)?.entry;
                        Some((
                            hash.clone(),
                            e.key.clone(),
                            e.query_shape_hash.clone(),
                            e.metrics.to_doc(),
                        ))
                    })
                    .collect()
            };
            let as_of = DateTime::now();
            for (hash, source, query_shape_hash, metrics) in copied {
                let key = match hmac_key {
                    None => source.debug.clone(),
                    Some(k) => match transformed_key(&source, k) {
                        Some(key) => key,
                        None => {
                            self.hmac_errors.fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                    },
                };
                out.push(doc! {
                    "key": key,
                    "keyHash": hash,
                    "queryShapeHash": query_shape_hash,
                    "metrics": metrics,
                    "asOf": as_of,
                });
            }
        }
        out
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

/// `$queryStats`' rows (its argument checked already): every entry, each
/// identifier as its HMAC when `transformIdentifiers` gives a key. With
/// the store's budget at 0 bytes, mongod 8.2.11's 6579000.
pub(crate) fn stage_rows(spec: Option<&Bson>) -> Result<Vec<Document>, crate::CommandError> {
    let Some(store) = CURRENT.with(|c| c.borrow().clone()) else {
        return Ok(Vec::new());
    };
    if !store.enabled() {
        return Err(crate::CommandError::new(
            6579000,
            "Location6579000",
            "Executor error during aggregate command on namespace: admin.$cmd.aggregate :: \
             caused by :: Query stats is not enabled without the feature flag on and a cache \
             size greater than 0 bytes",
        ));
    }
    let key = spec
        .and_then(Bson::as_document)
        .and_then(|s| s.get_document("transformIdentifiers").ok())
        .filter(|t| t.get_str("algorithm") == Ok("hmac-sha-256"))
        .and_then(|t| match t.get("hmacKey") {
            Some(Bson::Binary(b)) => Some(b.bytes.clone()),
            _ => None,
        });
    Ok(store.rows(key.as_deref()))
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
        "find" | "aggregate" | "count" | "distinct" | "getMore" | "explain"
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
    // `registerRequest`: nothing while the store has no budget, nothing for
    // a command that failed, and nothing the rate limiter turns away --
    // all checked before the shape, which every query would otherwise pay
    // for with query stats at their default, off.
    if !store.enabled() || reply.ok_number().unwrap_or(0.0) != 1.0 {
        return;
    }
    let Some((request, explain)) = recorded_request(request) else {
        return;
    };
    if !store.should_collect() {
        return;
    }
    let name = request.keys().next().map(String::as_str).unwrap_or(name);
    let Some(shape) = crate::query_shape::stats_shape(&ctx.db_name, &request) else {
        return;
    };
    let key = key(&request, ctx, &shape, explain);
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
    let never_exhausts = changes(&request);
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

/// The command a query is recorded as, and the verbosity of an `aggregate`
/// that explains: `{explain: true}` explains at `queryPlanner`, and the
/// `explain` command at its `verbosity` (`allPlansExecution` by default),
/// with its `comment` (measured 8.2.11, 2026-11-14). mongod records no
/// explained `find`, `distinct` or `count`.
fn recorded_request(request: &Document) -> Option<(Cow<'_, Document>, Option<String>)> {
    if let Ok(inner) = request.get_document("explain") {
        if inner.keys().next().map(String::as_str) != Some("aggregate") {
            return None;
        }
        let mut inner = inner.clone();
        if let (false, Some(comment)) = (inner.contains_key("comment"), request.get("comment")) {
            inner.insert("comment", comment.clone());
        }
        let verbosity = request
            .get_str("verbosity")
            .unwrap_or("allPlansExecution")
            .to_string();
        return Some((Cow::Owned(inner), Some(verbosity)));
    }
    if request.contains_key("explain") {
        let explains = request.get_bool("explain") == Ok(true);
        return Some((
            Cow::Borrowed(request),
            explains.then(|| "queryPlanner".to_string()),
        ));
    }
    Some((Cow::Borrowed(request), None))
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
fn key(
    request: &Document,
    ctx: &CommandContext,
    shape: &StatsShape,
    explain: Option<String>,
) -> Key {
    let shape_ns = shape.representative.get_document("cmdNs").ok();
    let coll = shape_ns.and_then(|n| n.get_str("coll").ok()).unwrap_or("");
    let parts = Parts {
        client: client(ctx),
        collection_type: collection_type(request, ctx, &ctx.db_name, coll),
        explain,
    };
    let representative = key_doc(request, &ctx.db_name, &parts, &shape.representative, false);
    let debug = key_doc(request, &ctx.db_name, &parts, &shape.debug, true);
    let bytes = crate::bsondoc::to_vec(&representative).unwrap_or_default();
    let hash = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&bytes));
    let command = request.keys().next().map(String::as_str).unwrap_or("");
    Key {
        size: entry_overhead(command) + bytes.len(),
        source: Arc::new(KeySource {
            debug,
            db: ctx.db_name.clone(),
            request: request.clone(),
            parts,
        }),
        hash,
        query_shape_hash: shape.hash.clone(),
    }
}

/// The key with each identifier as its HMAC under `hmac_key`
/// (`computeQueryStatsKey` with `transformIdentifiers`), or `None` when the
/// command no longer shapes.
fn transformed_key(source: &KeySource, hmac_key: &[u8]) -> Option<Document> {
    crate::query_shape::with_hmac(hmac_key, || {
        let shape = crate::query_shape::debug_shape(&source.db, &source.request)?;
        Some(key_doc(
            &source.request,
            &source.db,
            &source.parts,
            &shape,
            true,
        ))
    })
}

fn literal(v: &Bson, debug: bool) -> Bson {
    crate::query_shape::literal(v, debug).unwrap_or(Bson::Null)
}

fn key_doc(request: &Document, db: &str, parts: &Parts, shape: &Document, debug: bool) -> Document {
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
    if let Some(client) = &parts.client {
        key.insert("client", client.clone());
    }
    key.insert("collectionType", parts.collection_type);
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
                        .map(|(db, coll)| {
                            let (db, coll) = (
                                crate::query_shape::ident(&db),
                                crate::query_shape::ident(&coll),
                            );
                            Bson::Document(doc! { "db": db, "coll": coll })
                        })
                        .collect::<Vec<_>>(),
                );
            }
            if let Some(v) = request
                .get("bypassDocumentValidation")
                .and_then(Bson::as_bool)
            {
                key.insert("bypassDocumentValidation", v);
            }
            // The cursor's batch size, when the command has a cursor (one
            // that explains need not): mongod fills in its default.
            if request.contains_key("cursor") {
                key.insert(
                    "cursor",
                    doc! { "batchSize": literal(&Bson::Int64(0), debug) },
                );
            }
            if let Some(verbosity) = &parts.explain {
                key.insert("explain", verbosity.as_str());
            }
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

/// `extractHintShape`: an index's name or key pattern as given, its
/// fields (and a name) as field paths; `$natural` as it stands.
fn hint(request: &Document) -> Option<Document> {
    use crate::query_shape::field_path;
    match request.get("hint")? {
        Bson::String(name) => Some(doc! { "$hint": field_path(name) }),
        Bson::Document(d) if !d.is_empty() => Some(
            d.iter()
                .map(|(k, v)| match (k.as_str(), v) {
                    ("$natural", _) => (k.clone(), v.clone()),
                    ("$hint", Bson::String(name)) => (k.clone(), Bson::String(field_path(name))),
                    _ => (field_path(k), v.clone()),
                })
                .collect(),
        ),
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

    fn sized(hash: &str, size: usize) -> Key {
        let request = doc! { "find": "c", "filter": { "x": 1 } };
        Key {
            source: Arc::new(KeySource {
                debug: doc! { "queryShape": { "command": "find" } },
                db: "d".into(),
                request,
                parts: Parts {
                    client: None,
                    collection_type: "collection",
                    explain: None,
                },
            }),
            hash: hash.into(),
            query_shape_hash: "H".into(),
            size,
        }
    }

    fn key(hash: &str) -> Key {
        sized(hash, 100)
    }

    #[test]
    fn a_size_parses_as_mongods_memory_size_does() {
        assert_eq!(parse_size("1%"), Some((1.0, '%')));
        assert_eq!(parse_size(" 2 kb "), Some((2.0, 'K')));
        assert_eq!(parse_size("1.5GB"), Some((1.5, 'G')));
        assert_eq!(parse_size("10"), Some((10.0, 'B')));
        assert_eq!(parse_size("3."), Some((3.0, 'B')));
        for bad in ["", "abc", "5TB", "-1MB", "1e3MB", ".5MB", "1.2.3MB"] {
            assert_eq!(parse_size(bad), None, "{bad}");
        }
        assert_eq!(convert("1%", 32 << 30), 343_597_383);
        assert_eq!(convert("1.5MB", 0), 1_572_864);
        // At most 1 GB, and at most a quarter of memory.
        assert_eq!(cap_size(2 << 30, 32 << 30), 1 << 30);
        assert_eq!(cap_size(2 << 30, 4 << 30), 1 << 30);
        assert_eq!(cap_size(900 << 20, 2 << 30), 512 << 20);
    }

    #[test]
    fn the_partitions_are_one_per_16_mb_and_at_least_one_per_core() {
        assert_eq!(
            QueryStatsStore::with_size(343_597_383, 12).partitions.len(),
            21
        );
        assert_eq!(QueryStatsStore::with_size(1 << 20, 12).partitions.len(), 12);
        assert_eq!(QueryStatsStore::with_size(0, 4).partitions.len(), 4);
    }

    #[test]
    fn a_partition_over_its_share_evicts_its_least_recently_used() {
        // One partition of 1000 bytes.
        let store = QueryStatsStore::with_size(1000, 1);
        store.set_rate_limit(-1);
        store.write(&sized("a", 400), &snap(1, 1));
        store.write(&sized("b", 400), &snap(1, 1));
        // `a` is used again, so `b` is the least recently used.
        store.write(&sized("a", 400), &snap(1, 1));
        store.write(&sized("c", 400), &snap(1, 1));
        let hashes: Vec<String> = store
            .rows(None)
            .iter()
            .map(|r| r.get_str("keyHash").unwrap().to_string())
            .collect();
        assert_eq!(hashes, ["c", "a"]);
        let status = store.server_status();
        assert_eq!(status.get_i64("numEvicted").unwrap(), 1);
        assert_eq!(status.get_i64("numEntries").unwrap(), 2);
        assert_eq!(
            status.get_i64("queryStatsStoreSizeEstimateBytes").unwrap(),
            800
        );
        // Too large for the partition: in and out again, a write error.
        store.write(&sized("d", 2000), &snap(1, 1));
        let status = store.server_status();
        assert_eq!(status.get_i64("numQueryStatsStoreWriteErrors").unwrap(), 1);
        assert_eq!(status.get_i64("numEntries").unwrap(), 0);
    }

    #[test]
    fn a_smaller_cache_size_evicts_and_zero_turns_query_stats_off() {
        let store = QueryStatsStore::with_size(1000, 1);
        store.write(&sized("a", 400), &snap(1, 1));
        store.write(&sized("b", 400), &snap(1, 1));
        store.set_cache_size("500B");
        assert_eq!(store.rows(None).len(), 1);
        assert_eq!(store.server_status().get_i64("maxSizeBytes").unwrap(), 500);
        store.set_cache_size("0MB");
        assert!(!store.enabled());
        assert_eq!(store.rows(None).len(), 0);
        store.write(&sized("c", 1), &snap(1, 1));
        assert_eq!(store.rows(None).len(), 0);
    }

    #[test]
    fn the_sample_rate_takes_over_from_the_rate_limit() {
        let store = QueryStatsStore::new();
        store.set_sample_rate(1.0);
        assert!((0..50).all(|_| store.should_collect()));
        store.set_sample_rate(0.0005);
        assert_eq!(store.sample_rate.load(Ordering::Relaxed), 1);
        store.set_sample_rate(0.0);
        assert!(!store.should_collect());
        store.set_rate_limit(1);
        assert!(store.should_collect());
        assert!(!store.should_collect());
        assert_eq!(
            store
                .server_status()
                .get_i64("numRateLimitedRequests")
                .unwrap(),
            1
        );
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
        let rows = store.rows(None);
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
        assert!(store.rows(None).is_empty());
        // The last getMore exhausts it: closed while that getMore runs.
        store.get_more_begins(7);
        store.closed(7);
        assert!(store.rows(None).is_empty());
        store.get_more_ends(7, &snap(25, 1));
        let rows = store.rows(None);
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
        let rows = store.rows(None);
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
