//! `mongod`'s time-series bucket catalog: which bucket each measurement goes
//! to, kept in memory across the whole server.
//!
//! A transcription of `mongod` 8.2.11's
//! `src/mongo/db/timeseries/bucket_catalog/` (`bucket_catalog.cpp`,
//! `bucket_catalog_internal.cpp`, `bucket.cpp`, `measurement_map.cpp`,
//! `rollover.cpp`, `reopening.cpp`, `execution_stats.cpp`) and of the write
//! path that turns its batches into bucket documents
//! (`write_ops/timeseries_write_ops_utils_internal.cpp`). The order of every
//! decision is `mongod`'s:
//!
//! - an insert batch is split by meta value and each part sorted by time;
//! - a measurement goes to an open bucket of its series: soft-closed buckets
//!   (time forward) and archived ones (time backward) whose window holds it
//!   first, the fewer measurements the sooner, then the one open bucket;
//! - failing that, a bucket is reopened: an archived one from memory, or --
//!   only while every rollover this measurement met was for time running
//!   backwards, and only with an index on the meta and time -- the first
//!   stored bucket the reopening query finds; otherwise a new bucket;
//! - a bucket rolls over for time forward, time backward, count (1000), size
//!   (the cache-derived limit below) or a schema change, in that order.
//!
//! Size is `mongod`'s accounting, not a document size: the meta value once,
//! each new field's name and two copies of its first value, and after each
//! commit the compressed columns' bytes; before a commit only values over
//! 32 bytes count, at their uncompressed size. A reopened bucket starts from
//! its document's size. The limit is
//! `getCacheDerivedBucketMaxSize(storage cache, active buckets)`: 128000
//! bytes, less when the server holds so many buckets that the cache cannot
//! keep two per bucket of that size; a bucket of fewer than 10 measurements
//! may grow to 12 MB.
//!
//! Not modelled: closing idle buckets when the catalog's own memory passes
//! `timeseriesIdleBucketExpiryMemoryUsageThreshold`. That threshold counts
//! `mongod`'s C++ allocations, which this server does not make (backlog).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use bson::oid::ObjectId;
use bson::spec::BinarySubtype;
use bson::{doc, Binary, Bson, Document};
use secantus_core::bsoncolumn::{ColumnBuilder, Element};

use crate::storage::Storage;
use crate::ts_buckets::{fold, Layout, Schema};
use crate::CommandError;

/// `timeseriesBucketMaxCount`.
pub(crate) const MAX_COUNT: u64 = 1000;
/// `timeseriesBucketMaxSize`.
const MAX_SIZE: i32 = 128_000;
/// `timeseriesBucketMinSize`.
const MIN_SIZE: i32 = 5120;
/// `timeseriesBucketMinCount`.
const MIN_COUNT: u64 = 10;
/// `timeseriesLargeMeasurementThreshold`.
const LARGE_MEASUREMENT_THRESHOLD: i32 = 32;
/// `Bucket::kLargeMeasurementsMaxBucketSize`: 16 MB less 4 MB.
const LARGE_MEASUREMENTS_MAX_SIZE: i32 = 16 * 1024 * 1024 - 4 * 1024 * 1024;
/// `BSON("" << BSONObj()).objsize()`.
const EMPTY_OBJ_SIZE: i32 = 12;
/// `kTimeseriesControlCompressedSortedVersion`.
pub(crate) const SORTED_VERSION: i32 = 2;
/// `kTimeseriesControlCompressedUnsortedVersion`.
pub(crate) const UNSORTED_VERSION: i32 = 3;

/// `RolloverReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    None,
    TimeForward,
    TimeBackward,
    Count,
    SchemaChange,
    CachePressure,
    Size,
}

/// `RolloverAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    None,
    Archive,
    SoftClose,
    HardClose,
}

fn action(reason: Reason) -> Action {
    match reason {
        Reason::Count | Reason::SchemaChange | Reason::CachePressure | Reason::Size => {
            Action::HardClose
        }
        Reason::TimeForward => Action::SoftClose,
        Reason::TimeBackward => Action::Archive,
        Reason::None => Action::None,
    }
}

/// `ExecutionStats`: one counter per field, in the order
/// `appendExecutionStatsToBuilder` writes them.
// Every counter mongod reports is listed, those this server never moves
// (memory thresholds, eras, waits) included: they are reported as 0.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Stat {
    ActiveBuckets,
    BucketInserts,
    BucketUpdates,
    OpenedDueToMetadata,
    ClosedDueToCount,
    ClosedDueToSchemaChange,
    ClosedDueToSize,
    ClosedDueToTimeForward,
    ClosedDueToMemoryThreshold,
    Commits,
    Waits,
    MeasurementsCommitted,
    ClosedDueToReopening,
    ArchivedDueToMemoryThreshold,
    ArchivedDueToTimeBackward,
    Reopened,
    KeptOpenDueToLargeMeasurements,
    ClosedDueToCachePressure,
    Frozen,
    ConvertedToUnsorted,
    Fetched,
    Queried,
    FetchesFailed,
    QueriesFailed,
    ReopenFailedEraMismatch,
    ReopenFailedMalformedId,
    ReopenFailedHashCollision,
    ReopenFailedMarkedFrozen,
    ReopenFailedValidator,
    ReopenFailedMarkedClosed,
    ReopenFailedMinMax,
    ReopenFailedSchema,
    ReopenFailedUncompressedTime,
    ReopenFailedCompression,
    ReopenFailedWriteConflict,
    DuplicateReopened,
    TooLargeInsert,
    TooLargeUpdate,
}

const STAT_NAMES: [&str; 38] = [
    "numActiveBuckets",
    "numBucketInserts",
    "numBucketUpdates",
    "numBucketsOpenedDueToMetadata",
    "numBucketsClosedDueToCount",
    "numBucketsClosedDueToSchemaChange",
    "numBucketsClosedDueToSize",
    "numBucketsClosedDueToTimeForward",
    "numBucketsClosedDueToMemoryThreshold",
    "numCommits",
    "numWaits",
    "numMeasurementsCommitted",
    "numBucketsClosedDueToReopening",
    "numBucketsArchivedDueToMemoryThreshold",
    "numBucketsArchivedDueToTimeBackward",
    "numBucketsReopened",
    "numBucketsKeptOpenDueToLargeMeasurements",
    "numBucketsClosedDueToCachePressure",
    "numBucketsFrozen",
    "numCompressedBucketsConvertedToUnsorted",
    "numBucketsFetched",
    "numBucketsQueried",
    "numBucketFetchesFailed",
    "numBucketQueriesFailed",
    "numBucketReopeningsFailedDueToEraMismatch",
    "numBucketReopeningsFailedDueToMalformedIdField",
    "numBucketReopeningsFailedDueToHashCollision",
    "numBucketReopeningsFailedDueToMarkedFrozen",
    "numBucketReopeningsFailedDueToValidator",
    "numBucketReopeningsFailedDueToMarkedClosed",
    "numBucketReopeningsFailedDueToMinMaxCalculation",
    "numBucketReopeningsFailedDueToSchemaGeneration",
    "numBucketReopeningsFailedDueToUncompressedTimeColumn",
    "numBucketReopeningsFailedDueToCompressionFailure",
    "numBucketReopeningsFailedDueToWriteConflict",
    "numDuplicateBucketsReopened",
    "numBucketDocumentsTooLargeInsert",
    "numBucketDocumentsTooLargeUpdate",
];

/// A collection's (or the server's) execution statistics.
#[derive(Debug, Clone)]
pub(crate) struct ExecutionStats([i64; 38]);

impl Default for ExecutionStats {
    fn default() -> Self {
        ExecutionStats([0; 38])
    }
}

impl ExecutionStats {
    pub(crate) fn get(&self, stat: Stat) -> i64 {
        self.0[stat as usize]
    }

    /// `appendExecutionStatsToBuilder`: `appendNumber` writes an int32
    /// below 2^30 and an int64 from there.
    pub(crate) fn append_to(&self, out: &mut Document) {
        let number = |n: i64| {
            if n.unsigned_abs() < (1 << 30) {
                Bson::Int32(n as i32)
            } else {
                Bson::Int64(n)
            }
        };
        for (i, name) in STAT_NAMES.iter().enumerate() {
            out.insert(*name, number(self.0[i]));
            if i == Stat::MeasurementsCommitted as usize {
                let commits = self.get(Stat::Commits);
                if commits > 0 {
                    out.insert(
                        "avgNumMeasurementsPerCommit",
                        number(self.get(Stat::MeasurementsCommitted) / commits),
                    );
                }
            }
        }
    }
}

/// Counts for one collection and the server, moved together
/// (`ExecutionStatsController`).
struct StatsRef<'a> {
    coll: &'a mut ExecutionStats,
    global: &'a mut ExecutionStats,
}

impl StatsRef<'_> {
    fn add(&mut self, stat: Stat, n: i64) {
        self.coll.0[stat as usize] += n;
        self.global.0[stat as usize] += n;
    }

    fn inc(&mut self, stat: Stat) {
        self.add(stat, 1);
    }
}

/// An open bucket: what `mongod`'s `Bucket` holds, with its
/// `MeasurementMap` (one column builder per data field) and `MinMax`.
#[derive(Debug, Clone)]
struct Bucket {
    id: ObjectId,
    /// The series: the meta value's bytes.
    key: Vec<u8>,
    meta: Option<Bson>,
    /// The window's start (`control.min.<time>`), in milliseconds.
    min_time: i64,
    size: i32,
    /// Measurements staged or committed.
    count: u64,
    committed: u64,
    field_names: HashSet<String>,
    uncommitted_field_names: HashSet<String>,
    schema: Schema,
    rollover: Reason,
    kept_open_for_large: bool,
    sorted: bool,
    /// The data columns, in the order the bucket document holds them.
    columns: Vec<(String, ColumnBuilder)>,
    /// The columns' bytes after the last commit (`_compressedSize`).
    column_bytes: i32,
    /// Measurements in the columns.
    measurements_in_columns: u64,
    /// The time of the last committed measurement.
    last_time: Option<i64>,
    min: Document,
    max: Document,
    /// This operation's measurements for the bucket: `(input index, doc)`.
    staged: Vec<(usize, Document)>,
    staged_estimate: i32,
    new_field_names: Vec<String>,
    opened_due_to_metadata: bool,
}

/// One collection's buckets in memory.
#[derive(Default)]
struct Collection {
    open: Vec<Bucket>,
    /// Archived buckets, by series and window start (`stripe.archivedBuckets`).
    archived: BTreeMap<(Vec<u8>, i64), ObjectId>,
    frozen: HashSet<ObjectId>,
    stats: ExecutionStats,
}

#[derive(Default)]
struct Inner {
    /// By the bucket collection's UUID: a dropped and recreated collection
    /// starts afresh, as `mongod` keys its catalog.
    collections: HashMap<Vec<u8>, Collection>,
    global: ExecutionStats,
    /// `_bucketIdGenCounter`: seeded at random, one step per bucket.
    id_counter: u64,
}

/// The server's bucket catalog (`GlobalBucketCatalog`).
pub struct BucketCatalog {
    inner: Mutex<Inner>,
}

impl Default for BucketCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl BucketCatalog {
    pub fn new() -> Self {
        let mut seed = [0u8; 8];
        seed.copy_from_slice(&ObjectId::new().bytes()[4..12]);
        BucketCatalog {
            inner: Mutex::new(Inner {
                id_counter: u64::from_le_bytes(seed) ^ std::process::id() as u64,
                ..Inner::default()
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Forget a collection's buckets (`bucket_catalog::drop` / `clear`).
    pub(crate) fn clear(&self, uuid: &[u8]) {
        let mut inner = self.lock();
        if let Some(c) = inner.collections.remove(uuid) {
            let active = c.stats.get(Stat::ActiveBuckets);
            inner.global.0[Stat::ActiveBuckets as usize] -= active;
        }
    }

    /// Forget one bucket, written to other than through the catalog
    /// (`directWriteStart`: the bucket is cleared, and reopened from its
    /// document when it is next wanted).
    pub(crate) fn clear_bucket(&self, uuid: &[u8], id: &ObjectId) {
        let mut inner = self.lock();
        let Inner {
            collections,
            global,
            ..
        } = &mut *inner;
        let Some(c) = collections.get_mut(uuid) else {
            return;
        };
        let before = c.open.len();
        c.open.retain(|b| &b.id != id);
        let mut gone = (before - c.open.len()) as i64;
        let archived: Vec<_> = c
            .archived
            .iter()
            .filter(|(_, v)| *v == id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in archived {
            c.archived.remove(&k);
            gone += 1;
        }
        c.stats.0[Stat::ActiveBuckets as usize] -= gone;
        global.0[Stat::ActiveBuckets as usize] -= gone;
    }

    /// A collection's execution statistics, when it has any.
    pub(crate) fn collection_stats(&self, uuid: &[u8]) -> ExecutionStats {
        self.lock()
            .collections
            .get(uuid)
            .map(|c| c.stats.clone())
            .unwrap_or_default()
    }
}

/// A BSON value's size as an element value.
fn value_size(v: &Bson) -> i32 {
    crate::bsondoc::to_vec(&doc! {"": v.clone()})
        .map(|b| b.len() as i32 - 7)
        .unwrap_or(0)
}

/// `BSONElement::size()` for `name: v`.
fn element_size(name: &str, v: &Bson) -> i32 {
    2 + name.len() as i32 + value_size(v)
}

/// The `numDigits` of `bucket.cpp`: 0 for 0.
fn num_digits(mut n: u64) -> i32 {
    let mut d = 0;
    while n > 0 {
        n /= 10;
        d += 1;
    }
    d
}

/// `calculateBucketFieldsAndSizeChange`: `(verified, estimate, new field names)`.
fn size_change(bucket: &Bucket, doc: &Document, layout: &Layout) -> (i32, i32, Vec<String>) {
    let mut verified = 0;
    let mut estimate = 0;
    let mut new_fields = Vec::new();
    let digits = num_digits(bucket.count);
    for (name, v) in doc {
        if layout.meta.as_deref() == Some(name.as_str()) {
            // The meta value is kept once, at the top of the bucket.
            if bucket.size == 0 {
                verified += "meta".len() as i32 + 1 + value_size(v);
            }
            continue;
        }
        if !bucket.field_names.contains(name) {
            new_fields.push(name.clone());
            if !bucket.uncommitted_field_names.contains(name) {
                verified += EMPTY_OBJ_SIZE + name.len() as i32;
                verified += element_size(name, v) * 2;
            }
        }
        // The element under its position's name: the name's terminator
        // counts in `fieldNameSize`, not in the position.
        let size = element_size(name, v) - (name.len() as i32 + 1) + digits + 1;
        if size > LARGE_MEASUREMENT_THRESHOLD {
            estimate += size;
        }
    }
    (verified, estimate, new_fields)
}

/// `getCacheDerivedBucketMaxSize`: `(effective limit, derived limit)`.
fn cache_derived_max_size(cache_bytes: u64, active: i64) -> (i32, i32) {
    if active <= 0 {
        return (MAX_SIZE, i32::MAX);
    }
    let dynamic = cache_bytes / (2 * active as u64);
    let derived = (dynamic.min(i32::MAX as u64) as i32).max(MIN_SIZE);
    (MAX_SIZE.min(derived), derived)
}

/// Where a catalog decision reads the server's state.
struct Env<'a> {
    layout: &'a Layout,
    cache_bytes: u64,
}

/// `determineRolloverReason`. Takes the measurement into the bucket's
/// schema when it fits, as `schemaIncompatible` does.
fn rollover_reason(
    bucket: &mut Bucket,
    doc: &Document,
    t: i64,
    sizes: (i32, i32),
    active: i64,
    env: &Env<'_>,
    stats: &mut StatsRef<'_>,
) -> Reason {
    if t - bucket.min_time >= env.layout.span_ms {
        return Reason::TimeForward;
    }
    if t < bucket.min_time {
        return Reason::TimeBackward;
    }
    if bucket.count == MAX_COUNT {
        return Reason::Count;
    }
    let (effective, derived) = cache_derived_max_size(env.cache_bytes, active);
    let absolute = LARGE_MEASUREMENTS_MAX_SIZE.min(derived);
    let total = sizes.0 + sizes.1;
    if bucket.size + total > effective {
        if bucket.count < MIN_COUNT {
            if bucket.size + total > absolute {
                return if absolute != LARGE_MEASUREMENTS_MAX_SIZE {
                    Reason::CachePressure
                } else {
                    Reason::Size
                };
            }
            if !bucket.kept_open_for_large {
                bucket.kept_open_for_large = true;
                stats.inc(Stat::KeptOpenDueToLargeMeasurements);
            }
        } else if effective == MAX_SIZE {
            return Reason::Size;
        } else {
            return Reason::CachePressure;
        }
    }
    let held = bucket.schema.clone();
    if !bucket.schema.take(doc, env.layout) {
        bucket.schema = held;
        return Reason::SchemaChange;
    }
    Reason::None
}

/// `updateRolloverStats`.
fn rollover_stats(stats: &mut StatsRef<'_>, reason: Reason) {
    match reason {
        Reason::TimeBackward => stats.inc(Stat::ArchivedDueToTimeBackward),
        Reason::Count => stats.inc(Stat::ClosedDueToCount),
        Reason::CachePressure => stats.inc(Stat::ClosedDueToCachePressure),
        Reason::Size => stats.inc(Stat::ClosedDueToSize),
        Reason::SchemaChange => stats.inc(Stat::ClosedDueToSchemaChange),
        Reason::TimeForward => stats.inc(Stat::ClosedDueToTimeForward),
        Reason::None => {}
    }
}

/// What one insert operation needs besides the catalog.
pub(crate) struct Target<'a> {
    pub storage: &'a dyn Storage,
    pub db: &'a str,
    /// The time-series collection's name.
    pub coll: &'a str,
    pub layout: &'a Layout,
}

impl Target<'_> {
    fn buckets_ns(&self) -> String {
        format!("{}{}", crate::ts_buckets::BUCKETS_PREFIX, self.coll)
    }
}

/// A measurement's series key: its meta value, keys sorted, as bytes.
pub(crate) fn series_key(meta: Option<&Bson>) -> Vec<u8> {
    match meta {
        Some(m) => crate::bsondoc::to_vec(&doc! {"m": m.clone()}).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// One measurement's outcome: `Err` is a write error for it.
pub(crate) type Outcome = Result<(), CommandError>;

/// One measurement to place: its index in the caller's batch, the document.
pub(crate) struct Measurement {
    pub index: usize,
    pub doc: Document,
}

/// Insert `batches` into the time-series collection: each batch is staged
/// whole (split by meta value, each part sorted by time), then its buckets
/// are written. The outcome of each measurement, by its index.
pub(crate) fn insert(
    catalog: &BucketCatalog,
    target: &Target<'_>,
    batches: Vec<Vec<Measurement>>,
) -> Result<HashMap<usize, Outcome>, CommandError> {
    let buckets_ns = target.buckets_ns();
    let uuid = target
        .storage
        .collection_uuid(target.db, &buckets_ns)
        .map_err(crate::util::command_error)?;
    let env = Env {
        layout: target.layout,
        cache_bytes: target.storage.storage_cache_bytes(),
    };
    let mut outcomes = HashMap::new();
    let mut inner = catalog.lock();
    for batch in batches {
        stage_and_commit(&mut inner, &uuid, target, &env, batch, &mut outcomes)?;
    }
    Ok(outcomes)
}

/// `buildBatchedInsertContexts`: the batch by series, in the order each
/// series first appears, each in time order.
fn by_series(batch: Vec<Measurement>, layout: &Layout) -> Vec<(Vec<u8>, Vec<Measurement>)> {
    let mut groups: Vec<(Vec<u8>, Vec<Measurement>)> = Vec::new();
    for m in batch {
        let key = series_key(layout.meta_of(&m.doc).as_ref());
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(m),
            None => groups.push((key, vec![m])),
        }
    }
    for (_, g) in &mut groups {
        g.sort_by_key(|m| layout.time_of(&m.doc));
    }
    groups
}

fn stage_and_commit(
    inner: &mut Inner,
    uuid: &[u8],
    target: &Target<'_>,
    env: &Env<'_>,
    batch: Vec<Measurement>,
    outcomes: &mut HashMap<usize, Outcome>,
) -> Result<(), CommandError> {
    let mut touched: Vec<ObjectId> = Vec::new();
    for (key, group) in by_series(batch, target.layout) {
        stage_series(inner, uuid, target, env, &key, group, &mut touched)?;
    }
    for id in touched {
        commit(inner, uuid, target, env, &id, outcomes)?;
    }
    Ok(())
}

/// `stageInsertBatch` for one series.
fn stage_series(
    inner: &mut Inner,
    uuid: &[u8],
    target: &Target<'_>,
    env: &Env<'_>,
    key: &[u8],
    group: Vec<Measurement>,
    touched: &mut Vec<ObjectId>,
) -> Result<(), CommandError> {
    forget_changed(inner, uuid, target, key)?;
    let mut rest = group.into_iter().peekable();
    while let Some(first) = rest.peek() {
        let (at, opened_due_to_metadata) = eligible_bucket(inner, uuid, target, env, key, first)?;
        let Inner {
            collections,
            global,
            ..
        } = inner;
        let coll = collections.entry(uuid.to_vec()).or_default();
        let active = global.get(Stat::ActiveBuckets);
        let mut staged_any = false;
        while let Some(m) = rest.peek() {
            let mut stats = StatsRef {
                coll: &mut coll.stats,
                global,
            };
            let bucket = &mut coll.open[at];
            if !try_stage(bucket, m, active, env, &mut stats) {
                break;
            }
            let m = rest.next().expect("peeked");
            bucket.staged.push((m.index, m.doc));
            staged_any = true;
        }
        let bucket = &mut coll.open[at];
        bucket.opened_due_to_metadata = opened_due_to_metadata;
        if staged_any && !touched.contains(&bucket.id) {
            touched.push(bucket.id);
        }
    }
    Ok(())
}

/// Forget the series' open buckets that are no longer stored as the
/// catalog last wrote them: deleted (a TTL pass, a delete) or written by
/// hand. mongod clears such a bucket when the write happens
/// (`directWriteStart`); a write here that does not pass through the
/// commands (the TTL monitor) is found by reading the bucket back.
fn forget_changed(
    inner: &mut Inner,
    uuid: &[u8],
    target: &Target<'_>,
    key: &[u8],
) -> Result<(), CommandError> {
    let buckets_ns = target.buckets_ns();
    let Inner {
        collections,
        global,
        ..
    } = inner;
    let Some(coll) = collections.get_mut(uuid) else {
        return Ok(());
    };
    let ids: Vec<(ObjectId, u64)> = coll
        .open
        .iter()
        .filter(|b| b.key == key && b.committed > 0 && b.count == b.committed)
        .map(|b| (b.id, b.committed))
        .collect();
    for (id, committed) in ids {
        let rows = target
            .storage
            .find(target.db, &buckets_ns, &doc! {"_id": id}, None, None)
            .map_err(crate::util::command_error)?;
        let stored = rows.first().and_then(|r| decode(r).ok()).and_then(|d| {
            d.get_document("control")
                .ok()
                .and_then(|c| c.get("count"))
                .and_then(crate::util::as_i64)
        });
        if stored == Some(committed as i64) {
            continue;
        }
        coll.open.retain(|b| b.id != id);
        coll.stats.0[Stat::ActiveBuckets as usize] -= 1;
        global.0[Stat::ActiveBuckets as usize] -= 1;
    }
    Ok(())
}

/// `tryToInsertIntoBucketWithoutRollover`, with
/// `addMeasurementToBatchAndBucket`.
fn try_stage(
    bucket: &mut Bucket,
    m: &Measurement,
    active: i64,
    env: &Env<'_>,
    stats: &mut StatsRef<'_>,
) -> bool {
    let newly_opened = bucket.size == 0;
    let (verified, estimate, new_fields) = size_change(bucket, &m.doc, env.layout);
    let t = env.layout.time_of(&m.doc);
    if !newly_opened {
        let reason = rollover_reason(bucket, &m.doc, t, (verified, estimate), active, env, stats);
        if reason != Reason::None {
            bucket.rollover = reason;
            return false;
        }
    }
    for f in new_fields {
        if !bucket.new_field_names.contains(&f) {
            bucket.new_field_names.push(f.clone());
        }
        bucket.uncommitted_field_names.insert(f);
    }
    bucket.count += 1;
    bucket.staged_estimate += estimate;
    bucket.size += verified + estimate;
    if newly_opened {
        bucket.schema.take(&m.doc, env.layout);
    }
    true
}

/// `getEligibleBucket`: where the measurement goes, as an index into the
/// collection's open buckets, and whether its bucket was opened for its
/// meta value.
fn eligible_bucket(
    inner: &mut Inner,
    uuid: &[u8],
    target: &Target<'_>,
    env: &Env<'_>,
    key: &[u8],
    m: &Measurement,
) -> Result<(usize, bool), CommandError> {
    let t = env.layout.time_of(&m.doc);
    let mut allow_query = true;
    let mut opened_due_to_metadata = true;
    if let Some(at) = find_open_bucket(
        inner,
        uuid,
        env,
        key,
        m,
        t,
        &mut allow_query,
        &mut opened_due_to_metadata,
    ) {
        return Ok((at, opened_due_to_metadata));
    }
    if let Some(at) = reopen(
        inner,
        uuid,
        target,
        env,
        key,
        m,
        t,
        allow_query,
        &mut opened_due_to_metadata,
    )? {
        let Inner {
            collections,
            global,
            ..
        } = &mut *inner;
        let coll = collections.entry(uuid.to_vec()).or_default();
        let active = global.get(Stat::ActiveBuckets);
        let mut stats = StatsRef {
            coll: &mut coll.stats,
            global,
        };
        let bucket = &mut coll.open[at];
        let sizes = size_change(bucket, &m.doc, env.layout);
        let reason = rollover_reason(
            bucket,
            &m.doc,
            t,
            (sizes.0, sizes.1),
            active,
            env,
            &mut stats,
        );
        if reason == Reason::None {
            return Ok((at, opened_due_to_metadata));
        }
        bucket.rollover = reason;
    }
    let mut disallow = false;
    if let Some(at) = find_open_bucket(
        inner,
        uuid,
        env,
        key,
        m,
        t,
        &mut disallow,
        &mut opened_due_to_metadata,
    ) {
        return Ok((at, opened_due_to_metadata));
    }
    Ok((
        allocate(inner, uuid, env, key, m, t),
        opened_due_to_metadata,
    ))
}

/// `decideQueryBasedReopening`.
fn decide_query(reason: Reason, allow: &mut bool) {
    if reason != Reason::TimeBackward {
        *allow = false;
    }
}

/// `rollover`: act on a bucket's rollover reason now, if it has nothing
/// uncommitted; otherwise once its batch is written.
fn roll_over(
    coll: &mut Collection,
    global: &mut ExecutionStats,
    at: usize,
    reason: Reason,
) -> bool {
    let bucket = &coll.open[at];
    if bucket.count != bucket.committed {
        coll.open[at].rollover = reason;
        return false;
    }
    let mut stats = StatsRef {
        coll: &mut coll.stats,
        global,
    };
    rollover_stats(&mut stats, reason);
    let bucket = coll.open.remove(at);
    if action(reason) == Action::Archive {
        let slot = (bucket.key.clone(), bucket.min_time);
        if let std::collections::btree_map::Entry::Vacant(e) = coll.archived.entry(slot) {
            // Archived buckets stay active (`removeBucketWithoutStats`).
            e.insert(bucket.id);
            return true;
        }
    }
    // `closeOpenBucket`.
    coll.stats.0[Stat::ActiveBuckets as usize] -= 1;
    global.0[Stat::ActiveBuckets as usize] -= 1;
    true
}

/// `findOpenBucketForMeasurement` (with `findAndRolloverOpenBuckets`).
#[allow(clippy::too_many_arguments)]
fn find_open_bucket(
    inner: &mut Inner,
    uuid: &[u8],
    env: &Env<'_>,
    key: &[u8],
    m: &Measurement,
    t: i64,
    allow_query: &mut bool,
    opened_due_to_metadata: &mut bool,
) -> Option<usize> {
    let Inner {
        collections,
        global,
        ..
    } = inner;
    let coll = collections.entry(uuid.to_vec()).or_default();
    let mut none_bucket: Option<ObjectId> = None;
    let mut soft_closed: Vec<ObjectId> = Vec::new();
    let mut archived: Vec<ObjectId> = Vec::new();
    let ids: Vec<ObjectId> = coll
        .open
        .iter()
        .filter(|b| b.key == key)
        .map(|b| b.id)
        .collect();
    for id in ids {
        *opened_due_to_metadata = false;
        let Some(at) = coll.open.iter().position(|b| b.id == id) else {
            continue;
        };
        let reason = coll.open[at].rollover;
        match action(reason) {
            Action::None => none_bucket = Some(id),
            Action::HardClose => {
                roll_over(coll, global, at, reason);
                decide_query(reason, allow_query);
            }
            Action::Archive | Action::SoftClose => {
                let min_time = coll.open[at].min_time;
                if t >= min_time && t - min_time < env.layout.span_ms {
                    if action(reason) == Action::Archive {
                        archived.push(id);
                    } else {
                        soft_closed.push(id);
                    }
                } else {
                    roll_over(coll, global, at, reason);
                    decide_query(reason, allow_query);
                }
            }
        }
    }
    let count_of = |coll: &Collection, id: &ObjectId| {
        coll.open
            .iter()
            .find(|b| &b.id == id)
            .map_or(0, |b| b.count)
    };
    soft_closed.sort_by_key(|id| count_of(coll, id));
    archived.sort_by_key(|id| count_of(coll, id));
    let candidates = soft_closed.into_iter().chain(archived).chain(none_bucket);
    let active = global.get(Stat::ActiveBuckets);
    for id in candidates {
        let Some(at) = coll.open.iter().position(|b| b.id == id) else {
            continue;
        };
        let mut stats = StatsRef {
            coll: &mut coll.stats,
            global,
        };
        let bucket = &mut coll.open[at];
        let sizes = size_change(bucket, &m.doc, env.layout);
        let reason = rollover_reason(
            bucket,
            &m.doc,
            t,
            (sizes.0, sizes.1),
            active,
            env,
            &mut stats,
        );
        if reason == Reason::None {
            return Some(at);
        }
        bucket.rollover = reason;
        decide_query(reason, allow_query);
    }
    None
}

/// `allocateBucket`.
fn allocate(
    inner: &mut Inner,
    uuid: &[u8],
    env: &Env<'_>,
    key: &[u8],
    m: &Measurement,
    t: i64,
) -> usize {
    let rounding = env.layout.rounding_ms;
    let rounded = t.div_euclid(rounding) * rounding;
    inner.id_counter = inner.id_counter.wrapping_add(1);
    let mut bytes = [0u8; 12];
    bytes[..4].copy_from_slice(&(rounded.div_euclid(1000) as u32).to_be_bytes());
    bytes[4..].copy_from_slice(&inner.id_counter.to_be_bytes());
    let Inner {
        collections,
        global,
        ..
    } = inner;
    let coll = collections.entry(uuid.to_vec()).or_default();
    coll.stats.0[Stat::ActiveBuckets as usize] += 1;
    global.0[Stat::ActiveBuckets as usize] += 1;
    coll.open.push(Bucket {
        id: ObjectId::from_bytes(bytes),
        key: key.to_vec(),
        meta: env.layout.meta_of(&m.doc),
        min_time: rounded,
        size: 0,
        count: 0,
        committed: 0,
        field_names: HashSet::new(),
        uncommitted_field_names: HashSet::new(),
        schema: Schema::default(),
        rollover: Reason::None,
        kept_open_for_large: false,
        sorted: true,
        columns: Vec::new(),
        column_bytes: 0,
        measurements_in_columns: 0,
        last_time: None,
        // The summaries take each field where the measurements first
        // have it (measured 8.2.11); `control.min.<time>` is then the
        // window's start.
        min: Document::new(),
        max: Document::new(),
        staged: Vec::new(),
        staged_estimate: 0,
        new_field_names: Vec::new(),
        opened_due_to_metadata: false,
    });
    coll.open.len() - 1
}

/// `potentiallyReopenBucket`: an archived bucket whose window holds `t`,
/// or the reopening query's first bucket. Its index among the open
/// buckets.
#[allow(clippy::too_many_arguments)]
fn reopen(
    inner: &mut Inner,
    uuid: &[u8],
    target: &Target<'_>,
    env: &Env<'_>,
    key: &[u8],
    m: &Measurement,
    t: i64,
    allow_query: bool,
    opened_due_to_metadata: &mut bool,
) -> Result<Option<usize>, CommandError> {
    let buckets_ns = target.buckets_ns();
    let span = env.layout.span_ms;
    let archived = {
        let coll = inner.collections.entry(uuid.to_vec()).or_default();
        coll.archived
            .range((key.to_vec(), i64::MIN)..=(key.to_vec(), t))
            .next_back()
            .filter(|((_, start), _)| t - start < span)
            .map(|(_, id)| *id)
    };
    let found = if let Some(id) = archived {
        let rows = target
            .storage
            .find(target.db, &buckets_ns, &doc! {"_id": id}, None, None)
            .map_err(crate::util::command_error)?;
        let doc = rows.first().map(|r| decode(r)).transpose()?;
        let Inner {
            collections,
            global,
            ..
        } = &mut *inner;
        let coll = collections.entry(uuid.to_vec()).or_default();
        let mut stats = StatsRef {
            coll: &mut coll.stats,
            global,
        };
        stats.inc(if doc.is_some() {
            Stat::Fetched
        } else {
            Stat::FetchesFailed
        });
        doc
    } else if allow_query {
        let doc = query_candidate(inner, target, env, m, t)?;
        let Inner {
            collections,
            global,
            ..
        } = &mut *inner;
        let coll = collections.entry(uuid.to_vec()).or_default();
        let mut stats = StatsRef {
            coll: &mut coll.stats,
            global,
        };
        stats.inc(if doc.is_some() {
            Stat::Queried
        } else {
            Stat::QueriesFailed
        });
        doc
    } else {
        None
    };
    let Some(found) = found else {
        return Ok(None);
    };
    *opened_due_to_metadata = false;
    let Inner {
        collections,
        global,
        ..
    } = &mut *inner;
    let coll = collections.entry(uuid.to_vec()).or_default();
    let mut stats = StatsRef {
        coll: &mut coll.stats,
        global,
    };
    let Some(bucket) = rehydrate(&found, key, env.layout, &coll.frozen, &mut stats) else {
        return Ok(None);
    };
    if coll.open.iter().any(|b| b.id == bucket.id) {
        stats.inc(Stat::DuplicateReopened);
        return Ok(None);
    }
    // `loadBucketIntoCatalog`.
    let slot = (key.to_vec(), bucket.min_time);
    if coll.archived.remove(&slot).is_some() {
        stats.add(Stat::ActiveBuckets, -1);
    }
    if let Some(at) = coll
        .open
        .iter()
        .position(|b| b.key == key && b.rollover == Reason::None)
    {
        stats.inc(Stat::ClosedDueToReopening);
        if coll.open[at].count == coll.open[at].committed {
            coll.open.remove(at);
            coll.stats.0[Stat::ActiveBuckets as usize] -= 1;
            global.0[Stat::ActiveBuckets as usize] -= 1;
        } else {
            coll.open[at].rollover = Reason::TimeForward;
        }
    }
    let mut stats = StatsRef {
        coll: &mut coll.stats,
        global,
    };
    stats.inc(Stat::Reopened);
    stats.inc(Stat::ActiveBuckets);
    coll.open.push(bucket);
    Ok(Some(coll.open.len() - 1))
}

fn decode(raw: &[u8]) -> Result<Document, CommandError> {
    secantus_core::bsondoc::from_bytes(raw)
        .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))
}

/// `getIndexSupportingReopeningQuery`: a ready, non-partial index whose key
/// starts with `meta` (when there is a meta field) then
/// `control.min.<time>`.
fn reopening_index(target: &Target<'_>) -> Result<bool, CommandError> {
    let mut want: Vec<String> = Vec::new();
    if target.layout.meta.is_some() {
        want.push("meta".into());
    }
    want.push(format!("control.min.{}", target.layout.time));
    // The collection's indexes, keyed as they are on the buckets.
    let indexes = target
        .storage
        .list_indexes(target.db, target.coll)
        .map_err(crate::util::command_error)?;
    Ok(indexes.iter().any(|spec| {
        if spec
            .get_document("partialFilterExpression")
            .is_ok_and(|p| !p.is_empty())
        {
            return false;
        }
        let Ok(key) = spec.get_document("key") else {
            return false;
        };
        let key = crate::ts_buckets::bucket_key(key, target.layout);
        key.keys().take(want.len()).eq(want.iter()) && key.len() >= want.len()
    }))
}

/// `getQueryReopeningCandidate` + `reopenQueriedBucket`: the reopening
/// pipeline run with the supporting index's order.
fn query_candidate(
    inner: &mut Inner,
    target: &Target<'_>,
    env: &Env<'_>,
    m: &Measurement,
    t: i64,
) -> Result<Option<Document>, CommandError> {
    if !reopening_index(target)? {
        return Ok(None);
    }
    let layout = env.layout;
    let min_path = format!("control.min.{}", layout.time);
    let meta_filter = match layout.meta_of(&m.doc) {
        Some(meta) => doc! { "meta": meta },
        None => doc! { "meta": {"$exists": false} },
    };
    let filter = doc! { "$and": [
        { "$or": [ {"control.closed": {"$exists": false}}, {"control.closed": false} ] },
        { "$and": [
            { min_path.clone(): { "$lte": bson::DateTime::from_millis(t) } },
            { min_path.clone(): { "$gt": bson::DateTime::from_millis(t - layout.span_ms) } },
        ] },
        meta_filter,
        { format!("data.{}.{}", layout.time, MAX_COUNT - 1): { "$exists": false } },
    ] };
    let (max_size, _) =
        cache_derived_max_size(env.cache_bytes, inner.global.get(Stat::ActiveBuckets));
    let rows = target
        .storage
        .find(target.db, &target.buckets_ns(), &filter, None, None)
        .map_err(crate::util::command_error)?;
    let mut candidates: Vec<(Bson, Bson, Vec<u8>, Document)> = Vec::new();
    for raw in rows {
        if raw.len() as i64 >= i64::from(max_size) {
            continue;
        }
        let doc = decode(&raw)?;
        let min = doc
            .get_document("control")
            .ok()
            .and_then(|c| c.get_document("min").ok())
            .and_then(|c| c.get(&layout.time))
            .cloned()
            .unwrap_or(Bson::Null);
        let max = doc
            .get_document("control")
            .ok()
            .and_then(|c| c.get_document("max").ok())
            .and_then(|c| c.get(&layout.time))
            .cloned()
            .unwrap_or(Bson::Null);
        let id = match doc.get("_id") {
            Some(Bson::ObjectId(o)) => o.bytes().to_vec(),
            _ => Vec::new(),
        };
        candidates.push((min, max, id, doc));
    }
    // The index's order: the meta value is the same for all of them, then
    // the window's start, its end, and the record (the `_id`).
    candidates.sort_by(|a, b| {
        secantus_core::order::cmp(&a.0, &b.0)
            .then_with(|| secantus_core::order::cmp(&a.1, &b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    Ok(candidates.into_iter().next().map(|c| c.3))
}

/// `rehydrateBucket`: the in-memory bucket of a stored bucket document, or
/// `None` when it cannot be reopened.
fn rehydrate(
    doc: &Document,
    key: &[u8],
    layout: &Layout,
    frozen: &HashSet<ObjectId>,
    stats: &mut StatsRef<'_>,
) -> Option<Bucket> {
    let Some(Bson::ObjectId(id)) = doc.get("_id") else {
        stats.inc(Stat::ReopenFailedMalformedId);
        return None;
    };
    let meta = layout.meta.as_ref().and_then(|_| doc.get("meta").cloned());
    if series_key(meta.as_ref()) != key {
        stats.inc(Stat::ReopenFailedHashCollision);
        return None;
    }
    if frozen.contains(id) {
        stats.inc(Stat::ReopenFailedMarkedFrozen);
        return None;
    }
    let validator = crate::ts_buckets::validator(&layout.time);
    if !secantus_core::query::matches(doc, &validator, &Document::new(), None).unwrap_or(true) {
        stats.inc(Stat::ReopenFailedValidator);
        return None;
    }
    let control = doc.get_document("control").ok()?;
    if control.get_bool("closed").unwrap_or(false) {
        stats.inc(Stat::ReopenFailedMarkedClosed);
        return None;
    }
    let (Ok(min), Ok(max)) = (control.get_document("min"), control.get_document("max")) else {
        stats.inc(Stat::ReopenFailedMinMax);
        return None;
    };
    if min.is_empty() || max.is_empty() {
        stats.inc(Stat::ReopenFailedMinMax);
        return None;
    }
    let min_time = match min.get(&layout.time) {
        Some(Bson::DateTime(d)) => d.timestamp_millis(),
        _ => return None,
    };
    let mut schema = Schema::default();
    if !(schema.take_all(min) && schema.take_all(max)) {
        stats.inc(Stat::ReopenFailedSchema);
        return None;
    }
    let data = doc.get_document("data").ok()?;
    let Some(Bson::Binary(time_column)) = data.get(&layout.time) else {
        stats.inc(Stat::ReopenFailedUncompressedTime);
        return None;
    };
    if time_column.subtype != BinarySubtype::Column {
        stats.inc(Stat::ReopenFailedUncompressedTime);
        return None;
    }
    let count = match secantus_core::bsoncolumn::decode(&time_column.bytes) {
        Ok(values) => values.len() as u64,
        Err(_) => {
            stats.inc(Stat::ReopenFailedCompression);
            return None;
        }
    };
    // `MeasurementMap::initBuilders`: each column's builder reopened from
    // its bytes, which is the same as appending every value again.
    let mut columns = Vec::new();
    let mut column_bytes = 0;
    let mut last_time = None;
    for (name, v) in data {
        let Bson::Binary(b) = v else {
            stats.inc(Stat::ReopenFailedCompression);
            return None;
        };
        if b.subtype != BinarySubtype::Column {
            stats.inc(Stat::ReopenFailedCompression);
            return None;
        }
        let Ok(values) = secantus_core::bsoncolumn::decode(&b.bytes) else {
            stats.inc(Stat::ReopenFailedCompression);
            return None;
        };
        let mut builder = ColumnBuilder::new();
        for value in &values {
            builder.append(value.as_ref().map(|v| Element {
                ty: v.ty,
                value: &v.value,
            }));
        }
        if name == &layout.time {
            last_time = values.iter().rev().flatten().next().and_then(|v| {
                (v.ty == 0x09 && v.value.len() == 8)
                    .then(|| i64::from_le_bytes(v.value[..8].try_into().expect("8 bytes")))
            });
        }
        column_bytes += b.bytes.len() as i32;
        columns.push((name.clone(), builder));
    }
    let size = crate::bsondoc::to_vec(doc).map_or(0, |b| b.len() as i32);
    let version = control.get("version").and_then(crate::util::as_i64);
    Some(Bucket {
        id: *id,
        key: key.to_vec(),
        meta,
        min_time,
        size,
        count,
        committed: count,
        field_names: data.keys().cloned().collect(),
        uncommitted_field_names: HashSet::new(),
        schema,
        rollover: Reason::None,
        kept_open_for_large: false,
        sorted: version == Some(i64::from(SORTED_VERSION)),
        columns,
        column_bytes,
        measurements_in_columns: count,
        last_time,
        min: min.clone(),
        max: max.clone(),
        staged: Vec::new(),
        staged_estimate: 0,
        new_field_names: Vec::new(),
        opened_due_to_metadata: false,
    })
}

/// A value as a column element.
fn element_bytes(v: &Bson) -> Result<(u8, Vec<u8>), CommandError> {
    let bytes = crate::bsondoc::to_vec(&doc! {"": v.clone()})
        .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?;
    Ok((bytes[4], bytes[6..bytes.len() - 1].to_vec()))
}

/// `MeasurementMap::insertOne`: each data field's value into its column
/// (a new column starts with a skip per earlier measurement), a skip into
/// the columns the measurement leaves out.
fn insert_one(bucket: &mut Bucket, doc: &Document, layout: &Layout) -> Result<(), CommandError> {
    let before = bucket.measurements_in_columns;
    let mut seen: HashSet<&str> = HashSet::new();
    for (name, v) in doc {
        if layout.meta.as_deref() == Some(name.as_str()) {
            continue;
        }
        if !seen.insert(name.as_str()) {
            return Err(CommandError::new(
                12602102,
                "Location12602102",
                "Measurements with duplicate field names cannot be stored in timeseries \
                 collections",
            ));
        }
        let (ty, value) = element_bytes(v)?;
        let element = Some(Element { ty, value: &value });
        match bucket.columns.iter_mut().find(|(n, _)| n == name) {
            Some((_, builder)) => builder.append(element),
            None => {
                let mut builder = ColumnBuilder::new();
                for _ in 0..before {
                    builder.skip();
                }
                builder.append(element);
                bucket.columns.push((name.clone(), builder));
            }
        }
    }
    for (name, builder) in &mut bucket.columns {
        if !seen.contains(name.as_str()) {
            builder.skip();
        }
    }
    bucket.measurements_in_columns += 1;
    Ok(())
}

/// `MinMax::update` for one measurement: each data field folded into the
/// running minimum and maximum, a field met for the first time going last.
fn update_min_max(bucket: &mut Bucket, doc: &Document, layout: &Layout) {
    for (name, v) in doc {
        if layout.meta.as_deref() == Some(name.as_str()) {
            continue;
        }
        for (acc, want_max) in [(&mut bucket.min, false), (&mut bucket.max, true)] {
            match acc.get_mut(name) {
                Some(slot) => {
                    let held = std::mem::replace(slot, Bson::Null);
                    *slot = fold(Some(held), v, want_max);
                }
                None => {
                    acc.insert(name.clone(), fold(None, v, want_max));
                }
            }
        }
    }
}

/// The bucket document the bucket's columns and summaries make.
fn bucket_document(bucket: &Bucket) -> Result<Document, CommandError> {
    let mut data = Document::new();
    for (name, builder) in &bucket.columns {
        data.insert(
            name.clone(),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Column,
                bytes: builder.clone().finalize(),
            }),
        );
    }
    let version = if bucket.sorted {
        SORTED_VERSION
    } else {
        UNSORTED_VERSION
    };
    let mut doc = doc! {
        "_id": bucket.id,
        "control": {
            "version": version,
            "min": bucket.min.clone(),
            "max": bucket.max.clone(),
            "count": bucket.measurements_in_columns as i32,
        },
    };
    if let Some(meta) = &bucket.meta {
        doc.insert("meta", meta.clone());
    }
    doc.insert("data", data);
    Ok(doc)
}

/// Write the measurements a bucket has staged: a new bucket is inserted,
/// one with measurements already stored is replaced (`mongod` sends the
/// difference; the document that results is this one). Then `finish`.
fn commit(
    inner: &mut Inner,
    uuid: &[u8],
    target: &Target<'_>,
    env: &Env<'_>,
    id: &ObjectId,
    outcomes: &mut HashMap<usize, Outcome>,
) -> Result<(), CommandError> {
    let layout = env.layout;
    let Inner {
        collections,
        global,
        ..
    } = inner;
    let coll = collections.entry(uuid.to_vec()).or_default();
    let Some(at) = coll.open.iter().position(|b| &b.id == id) else {
        return Ok(());
    };
    let bucket = &mut coll.open[at];
    let staged = std::mem::take(&mut bucket.staged);
    if staged.is_empty() {
        return Ok(());
    }
    // `prepareWriteBatchForCommit`.
    let previously = bucket.committed;
    for f in std::mem::take(&mut bucket.new_field_names) {
        bucket.uncommitted_field_names.remove(&f);
        bucket.field_names.insert(f);
    }
    let mut converted_to_unsorted = false;
    if previously > 0 && bucket.sorted {
        let first = layout.time_of(&staged[0].1);
        if bucket.last_time.is_some_and(|last| first < last) {
            bucket.sorted = false;
            converted_to_unsorted = true;
        }
    }
    let mut failed: Option<CommandError> = None;
    for (_, d) in &staged {
        update_min_max(bucket, d, layout);
        if let Err(e) = insert_one(bucket, d, layout) {
            failed = Some(e);
            break;
        }
    }
    if let Some(slot) = bucket.min.get_mut(&layout.time) {
        *slot = Bson::DateTime(bson::DateTime::from_millis(bucket.min_time));
    }
    let written = match failed {
        Some(e) => Err(e),
        None => bucket_document(bucket).and_then(|doc| {
            let bytes = crate::bsondoc::to_vec(&doc)
                .map_err(|e| CommandError::new(8, "UnknownError", e.to_string()))?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(CommandError::new(
                    10334,
                    "BSONObjectTooLarge",
                    format!("BSONObj size: {} (0x{:X}) is invalid. Size must be between 0 and 16793600(16MB)", bytes.len(), bytes.len()),
                ));
            }
            let buckets_ns = target.buckets_ns();
            if previously == 0 {
                let (_, errors) = target
                    .storage
                    .insert(target.db, &buckets_ns, vec![bytes], true)
                    .map_err(crate::util::command_error)?;
                if let Some(e) = errors.first() {
                    return Err(write_error(e));
                }
            } else {
                let outcome = target
                    .storage
                    .update_matching(
                        target.db,
                        &buckets_ns,
                        &doc! {"_id": bucket.id},
                        &doc,
                        false,
                        false,
                    )
                    .map_err(crate::util::command_error)?;
                // Removed between the check above and this write.
                if outcome.matched == 0 {
                    return Err(CommandError::new(
                        112,
                        "WriteConflict",
                        "the time-series bucket was removed while it was written",
                    ));
                }
            }
            Ok(())
        }),
    };
    let mut stats = StatsRef {
        coll: &mut coll.stats,
        global,
    };
    match written {
        Ok(()) => {
            let bucket = &mut coll.open[at];
            let column_bytes: i32 = bucket
                .columns
                .iter()
                .map(|(_, b)| b.clone().finalize().len() as i32)
                .sum();
            // `finish`.
            bucket.size -= bucket.staged_estimate;
            bucket.size += column_bytes - bucket.column_bytes;
            bucket.column_bytes = column_bytes;
            bucket.staged_estimate = 0;
            bucket.committed += staged.len() as u64;
            bucket.last_time = staged.last().map(|(_, d)| layout.time_of(d));
            if converted_to_unsorted {
                stats.inc(Stat::ConvertedToUnsorted);
            }
            stats.inc(Stat::Commits);
            stats.inc(if previously == 0 {
                Stat::BucketInserts
            } else {
                Stat::BucketUpdates
            });
            if bucket.opened_due_to_metadata {
                stats.inc(Stat::OpenedDueToMetadata);
            }
            stats.add(Stat::MeasurementsCommitted, staged.len() as i64);
            for (index, _) in &staged {
                outcomes.insert(*index, Ok(()));
            }
            let reason = bucket.rollover;
            if action(reason) != Action::None {
                roll_over(coll, global, at, reason);
            }
        }
        Err(e) => {
            // The write failed: the bucket leaves the catalog (`abort`), and
            // is reopened from what is stored when it is next wanted.
            coll.open.remove(at);
            coll.stats.0[Stat::ActiveBuckets as usize] -= 1;
            global.0[Stat::ActiveBuckets as usize] -= 1;
            for (index, _) in &staged {
                outcomes.insert(*index, Err(e.clone()));
            }
        }
    }
    Ok(())
}

/// A storage write error document as a command error.
fn write_error(e: &Document) -> CommandError {
    CommandError::new(
        e.get_i32("code").unwrap_or(8),
        "",
        e.get_str("errmsg")
            .unwrap_or("bucket write failed")
            .to_string(),
    )
}
