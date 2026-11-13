//! Per-stage memory accounting, as mongod 8.2 keeps it (RM-211).
//!
//! mongod gives every blocking aggregation stage a memory budget and does
//! one of two things when a stage goes over it: spill to disk, when the
//! command allows it (`allowDiskUse`, true by default since
//! `allowDiskUseByDefault`), or refuse with the stage's own error. Some
//! budgets cannot be met by spilling at all -- one accumulator's array, one
//! `$lookup` result, one `$facet` document -- and those always refuse. The
//! budgets this server keeps, every one 100 MiB on 8.2:
//!
//! | budget | mongod knob | over it |
//! | --- | --- | --- |
//! | a `$group`'s groups | `internalDocumentSourceGroupMaxMemoryBytes` | spill, or 292 |
//! | one `$push` / `$addToSet` / `$concatArrays` / `$setUnion` | `internalQueryMaxPushBytes`, `internalQueryMaxAddToSetBytes`, ... | 146 |
//! | a `$setWindowFields` partition's documents | `internalDocumentSourceSetWindowFieldsMaxMemoryBytes` | spill, or 5643011 |
//! | one `$lookup` result | `internalLookupStageIntermediateDocumentMaxSizeBytes` | 4568 |
//! | one `$facet` document | `internalQueryFacetMaxOutputDocSizeBytes` | 4031700 |
//!
//! The sizes are mongod's own estimates, not the BSON size: a classic
//! `Value` is 16 bytes plus what it points to, a string 24 more for its
//! `RCString` header; an SBE value is a tag and a word (9 bytes) plus its
//! out-of-line body. They were read from mongod 8.2.1's source
//! (`Value::getApproximateSize`, `AccumulatorAddToSet`, `DocumentSourceFacet`,
//! `DocumentSourceLookUp`, `vm::ByteCode::addToArrayCapped`) and the two
//! constants the source cannot give -- a document's fixed overhead (104
//! bytes) and the `$addToSet` accumulator's own size (144 bytes) -- measured
//! against mongod 8.2.11 (2026-10-26) from the byte counts in its errors over
//! 1 MB and 500 kB strings. A document is sized as mongod sizes one read from
//! storage (its BSON and the fixed overhead); one the classic engine has
//! built up field by field is sized slightly differently there.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use bson::{Bson, Document};

/// Every budget above: 100 MiB.
pub const MAX_BYTES: usize = 104_857_600;

/// What mongod adds to a document's BSON size for its `Document` storage
/// (measured 8.2.11: a `{_id, s}` of 1,000,022 BSON bytes is 1,000,126).
pub const DOCUMENT_OVERHEAD: usize = 104;

/// What a document's metadata adds once `$sort` attaches its sort key,
/// beyond the key's own `Value`: `sizeof(MetadataHolder)` and the empty
/// fields `DocumentMetadataFields::getApproximateSize` counts. Measured on
/// mongod 8.2.1 (macOS arm64) and 8.2.12 (Linux x86-64 and arm64), which
/// agree: a document a `$rank` caches is its BSON + 104 + 255 + its key.
pub const SORT_KEY_METADATA: usize = 255;

/// `sizeof(Value)` in the classic engine.
pub(crate) const VALUE: usize = 16;
/// `sizeof(RCString)`: refcount, size and vtable (measured: a classic
/// string `Value` of n bytes is n + 40).
const RC_STRING: usize = 24;
/// `sizeof(RCVector<Value>)`.
pub(crate) const RC_VECTOR: usize = 40;
/// `sizeof(RCDecimal)`.
const RC_DECIMAL: usize = 32;
/// The longest string a classic `Value` keeps inline.
const SHORT_STR: usize = 11;
/// An SBE value's tag and word.
const SBE_VALUE: usize = 9;
/// The longest string SBE keeps in the word (`StringSmall`).
const SBE_SMALL_STR: usize = 7;
/// What a classic accumulator starts at (`sizeof(*this)`; measured for
/// `$addToSet`, and used for the others, whose size the errors never show).
pub const ACCUMULATOR_BASE: usize = 144;
/// `sizeof(AccumulatorPush)` (and `$concatArrays`'s), measured the same way.
pub const PUSH_BASE: usize = 120;

/// The function sizes a mongod build counts that follow from how it was
/// built -- its standard library -- rather than from what the function
/// holds. One per mongod build, chosen by the target this server is built
/// for: each platform's mongod is the reference (Joe, 2026-11-07).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Layout {
    /// A classic `$sum` / `$count` accumulator (`AccumulatorSum`).
    pub sum: usize,
    /// A removable window function's own size, before the values in its
    /// window: `$sum` and the functions sized as it is, `$avg`, `$push`,
    /// `$stdDevPop` / `$stdDevSamp`.
    pub removable: usize,
    pub removable_avg: usize,
    pub removable_push: usize,
    pub removable_std_dev: usize,
    /// `WindowFunctionMinMax` (`$min` / `$max`), before the values its
    /// multiset and its executor's queue hold (RM-404: macOS 8.2.1 112,
    /// from `maxFunctionMemoryUsageBytes` over windows of one to eleven
    /// documents; Linux 8.2.12 128, from the least budget of a `[-3, 3]`
    /// window, 852).
    pub removable_min_max: usize,
    /// `WindowFunctionIntegral` (a removable `$integral`), before the points
    /// in its window, and `AccumulatorIntegral` (one with no lower end),
    /// which holds one: they differ by platform only in the `std::deque`
    /// of points the function keeps (libc++ 48 bytes, libstdc++ 80). RM-509:
    /// macOS 8.2.11's `maxFunctionMemoryUsageBytes` 272 and 368, which
    /// `sizeof` over r8.2.11's members reproduces with libc++'s deque.
    pub removable_integral: usize,
    pub integral: usize,
    /// `AccumulatorRank`, `AccumulatorDenseRank`,
    /// `AccumulatorDocumentNumber`.
    pub rank: usize,
    pub dense_rank: usize,
    pub document_number: usize,
}

/// libstdc++: Linux mongod 8.2.12, x86-64 and arm64 alike (2026-11-08,
/// RM-327), the exemplar CI measures.
pub(crate) const LIBSTDCXX: Layout = Layout {
    sum: 176,
    removable: 296,
    removable_avg: 304,
    removable_push: 160,
    removable_std_dev: 472,
    removable_min_max: 128,
    removable_integral: 304,
    integral: 400,
    rank: 376,
    dense_rank: 368,
    document_number: 368,
};

/// libc++ on arm64: macOS mongod 8.2.1 and 8.2.11, which agree (2026-11-10,
/// RM-403: `maxAccumulatorMemoryUsageBytes`, `maxFunctionMemoryUsageBytes`
/// over windows of one to 21 documents with and without `partitionBy`, and
/// the least budgets). A removable function's maximum there is its size
/// with one value more than its window holds -- the executor adds the
/// value entering before it removes the one leaving -- so `$sum` over
/// `[-5, 5]` peaks at 304 + 12 x 16. The other functions' sizes are
/// Linux's.
pub(crate) const LIBCXX_ARM64: Layout = Layout {
    sum: 184,
    removable: 304,
    removable_avg: 312,
    removable_push: 128,
    removable_std_dev: 488,
    removable_min_max: 112,
    removable_integral: 272,
    integral: 368,
    rank: 344,
    dense_rank: 336,
    document_number: 336,
};

/// The layout of the mongod build this server stands in for: macOS arm64's
/// where it is built for one, Linux's everywhere else.
pub(crate) const LAYOUT: Layout = if crate::stl::LIBCXX {
    LIBCXX_ARM64
} else {
    LIBSTDCXX
};

/// Each budget, in bytes: mongod's runtime-settable knobs, 100 MiB unless
/// `setParameter` changed one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    /// `internalDocumentSourceGroupMaxMemoryBytes`: a classic `$group`.
    pub group: usize,
    /// `internalQuerySBEAggApproxMemoryUseInBytesBeforeSpill`: an SBE
    /// `$group`. Not settable at runtime on mongod 8.2.11, so no parameter
    /// sets it here either.
    pub sbe_group: usize,
    /// `internalQueryMaxPushBytes`
    pub push: usize,
    /// `internalQueryMaxAddToSetBytes`
    pub add_to_set: usize,
    /// `internalQueryMaxConcatArraysBytes`
    pub concat_arrays: usize,
    /// `internalQueryMaxSetUnionBytes`
    pub set_union: usize,
    /// `internalDocumentSourceSetWindowFieldsMaxMemoryBytes`
    pub window_fields: usize,
    /// `internalLookupStageIntermediateDocumentMaxSizeBytes`
    pub lookup: usize,
    /// `internalQueryFacetMaxOutputDocSizeBytes`
    pub facet: usize,
    /// `internalQueryMaxBlockingSortMemoryUsageBytes`: a blocking sort.
    pub sort: usize,
    /// `internalDocumentSourceBucketAutoMaxMemoryBytes`: `$bucketAuto`'s sort.
    pub bucket_auto: usize,
}

impl Default for Budgets {
    fn default() -> Self {
        Budgets {
            group: MAX_BYTES,
            sbe_group: MAX_BYTES,
            push: MAX_BYTES,
            add_to_set: MAX_BYTES,
            concat_arrays: MAX_BYTES,
            set_union: MAX_BYTES,
            window_fields: MAX_BYTES,
            lookup: MAX_BYTES,
            facet: MAX_BYTES,
            sort: MAX_BYTES,
            bucket_auto: MAX_BYTES,
        }
    }
}

impl Budgets {
    /// The parameter names, each with the field it sets.
    pub const PARAMETERS: [&'static str; 10] = [
        "internalQueryMaxBlockingSortMemoryUsageBytes",
        "internalDocumentSourceBucketAutoMaxMemoryBytes",
        "internalDocumentSourceGroupMaxMemoryBytes",
        "internalQueryMaxPushBytes",
        "internalQueryMaxAddToSetBytes",
        "internalQueryMaxConcatArraysBytes",
        "internalQueryMaxSetUnionBytes",
        "internalDocumentSourceSetWindowFieldsMaxMemoryBytes",
        "internalLookupStageIntermediateDocumentMaxSizeBytes",
        "internalQueryFacetMaxOutputDocSizeBytes",
    ];

    /// Set the budget the parameter `name` names; false for another name.
    pub fn set(&mut self, name: &str, bytes: usize) -> bool {
        let slot = match name {
            "internalDocumentSourceGroupMaxMemoryBytes" => &mut self.group,
            "internalQueryMaxPushBytes" => &mut self.push,
            "internalQueryMaxAddToSetBytes" => &mut self.add_to_set,
            "internalQueryMaxConcatArraysBytes" => &mut self.concat_arrays,
            "internalQueryMaxSetUnionBytes" => &mut self.set_union,
            "internalDocumentSourceSetWindowFieldsMaxMemoryBytes" => &mut self.window_fields,
            "internalLookupStageIntermediateDocumentMaxSizeBytes" => &mut self.lookup,
            "internalQueryFacetMaxOutputDocSizeBytes" => &mut self.facet,
            "internalQueryMaxBlockingSortMemoryUsageBytes" => &mut self.sort,
            "internalDocumentSourceBucketAutoMaxMemoryBytes" => &mut self.bucket_auto,
            _ => return false,
        };
        *slot = bytes;
        true
    }
}

/// Restores the previous budgets when dropped.
pub struct BudgetScope(Budgets);

impl Drop for BudgetScope {
    fn drop(&mut self) {
        BUDGETS.with(|b| b.set(self.0));
    }
}

/// Run what follows under `budgets`.
pub fn with_budgets(budgets: Budgets) -> BudgetScope {
    BudgetScope(BUDGETS.with(|b| b.replace(budgets)))
}

/// The budgets in force.
pub fn budgets() -> Budgets {
    BUDGETS.with(Cell::get)
}

thread_local! {
    static BUDGETS: Cell<Budgets> = const { Cell::new(Budgets {
        group: MAX_BYTES,
        sbe_group: MAX_BYTES,
        push: MAX_BYTES,
        add_to_set: MAX_BYTES,
        concat_arrays: MAX_BYTES,
        set_union: MAX_BYTES,
        window_fields: MAX_BYTES,
        lookup: MAX_BYTES,
        facet: MAX_BYTES,
        sort: MAX_BYTES,
        bucket_auto: MAX_BYTES,
    }) };
    static ALLOW_DISK_USE: Cell<bool> = const { Cell::new(true) };
    static REPORT: RefCell<SpillReport> = RefCell::new(SpillReport::default());
    /// Whether a stage's own counters are kept: only while `explain` runs
    /// a stage to read them ([`captured`]), so a `$group` in a `$lookup`
    /// does not keep one record for every document it is run for.
    static STAGE_STATS: Cell<bool> = const { Cell::new(false) };
    static CURSOR_READ: RefCell<Option<CursorRead>> = const { RefCell::new(None) };
    static SPILL_DIR: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// What the operation running on this thread wrote to disk, for its
/// profile entry and slow-query line.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpillReport {
    pub used_disk: bool,
    /// A blocking sort ran (the profile's `hasSortStage`).
    pub has_sort_stage: bool,
    pub group_spills: i64,
    pub group_spilled_records: i64,
    pub group_spilled_bytes: i64,
    pub group_spilled_data_storage_size: i64,
    pub sort_spills: i64,
    pub sort_spilled_records: i64,
    pub sort_spilled_bytes: i64,
    pub sort_spilled_data_storage_size: i64,
    /// What the sorts counted, spilled or not (`sortTotalDataSizeBytes`).
    pub sort_total_data_size: i64,
    /// The keys the sorts sorted (mongod's `keysSorted`).
    pub keys_sorted: i64,
    /// Each `$group` that finished, in the order they did (RM-328).
    pub groups: Vec<GroupStats>,
    /// Each `$setWindowFields` that finished, in the order they did.
    pub windows: Vec<WindowStats>,
}

/// What one `$group` counted, as mongod 8.2's `explain` lists it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GroupStats {
    /// Each accumulator's most memory over all its groups at once
    /// (`maxAccumulatorMemoryUsageBytes`), in the spec's order.
    pub max_accumulator_memory: Vec<(String, i64)>,
    /// The size of every document it returned (`totalOutputDataSizeBytes`).
    pub total_output_data_size: i64,
}

/// What one `$setWindowFields` counted, as mongod 8.2's `explain` lists it
/// for the `$_internalSetWindowFields` it runs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WindowStats {
    /// Each output's function at its largest (`maxFunctionMemoryUsageBytes`).
    pub max_function_memory: Vec<(String, i64)>,
    /// The stage's cache, functions and partition state at their largest
    /// together (`maxTotalMemoryUsageBytes`).
    pub max_total_memory: i64,
    pub spills: i64,
    pub spilled_records: i64,
    pub spilled_bytes: i64,
    pub spilled_data_storage_size: i64,
}

impl SpillReport {
    /// What `self` spilled beyond `before` (an earlier report of the same
    /// run, cut shorter).
    pub fn less(&self, before: &SpillReport) -> SpillReport {
        SpillReport {
            used_disk: self.used_disk && !before.used_disk,
            has_sort_stage: self.has_sort_stage,
            group_spills: self.group_spills - before.group_spills,
            group_spilled_records: self.group_spilled_records - before.group_spilled_records,
            group_spilled_bytes: self.group_spilled_bytes - before.group_spilled_bytes,
            group_spilled_data_storage_size: self.group_spilled_data_storage_size
                - before.group_spilled_data_storage_size,
            sort_spills: self.sort_spills - before.sort_spills,
            sort_spilled_records: self.sort_spilled_records - before.sort_spilled_records,
            sort_spilled_bytes: self.sort_spilled_bytes - before.sort_spilled_bytes,
            sort_spilled_data_storage_size: self.sort_spilled_data_storage_size
                - before.sort_spilled_data_storage_size,
            sort_total_data_size: self.sort_total_data_size - before.sort_total_data_size,
            keys_sorted: self.keys_sorted - before.keys_sorted,
            groups: self.groups[before.groups.len().min(self.groups.len())..].to_vec(),
            windows: self.windows[before.windows.len().min(self.windows.len())..].to_vec(),
        }
    }
}

/// One counter of `serverStatus`'s `metrics.query`, for the life of the
/// process.
pub struct Counter(std::sync::atomic::AtomicI64);

impl Counter {
    const fn new() -> Self {
        Counter(std::sync::atomic::AtomicI64::new(0))
    }

    fn add(&self, n: i64) {
        self.0.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn get(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// `metrics.query.<stage>`'s four spill counters (`group`,
/// `setWindowFields`, `bucketAuto`).
pub struct SpillCounters {
    pub spills: Counter,
    pub spilled_records: Counter,
    pub spilled_bytes: Counter,
    pub spilled_data_storage_size: Counter,
}

impl SpillCounters {
    const fn new() -> Self {
        SpillCounters {
            spills: Counter::new(),
            spilled_records: Counter::new(),
            spilled_bytes: Counter::new(),
            spilled_data_storage_size: Counter::new(),
        }
    }

    fn add(&self, spills: i64, records: i64, bytes: i64, storage: i64) {
        self.spills.add(spills);
        self.spilled_records.add(records);
        self.spilled_bytes.add(bytes);
        self.spilled_data_storage_size.add(storage);
    }
}

/// `metrics.query.sort`.
pub struct SortCounters {
    pub spill_to_disk: Counter,
    pub spill_to_disk_bytes: Counter,
    pub total_bytes_sorted: Counter,
    pub total_keys_sorted: Counter,
}

pub static GROUP_METRICS: SpillCounters = SpillCounters::new();
pub static WINDOW_METRICS: SpillCounters = SpillCounters::new();
pub static BUCKET_AUTO_METRICS: SpillCounters = SpillCounters::new();
pub static SORT_METRICS: SortCounters = SortCounters {
    spill_to_disk: Counter::new(),
    spill_to_disk_bytes: Counter::new(),
    total_bytes_sorted: Counter::new(),
    total_keys_sorted: Counter::new(),
};

/// A sort that ran: `keys` documents, `bytes` counted, in memory or not.
/// `metrics.query.sort` counts them when the operation ends
/// ([`count_sorted`]).
pub(crate) fn record_sort(keys: i64, bytes: i64) {
    REPORT.with(|r| {
        let mut r = r.borrow_mut();
        r.has_sort_stage = true;
        r.sort_total_data_size += bytes;
        r.keys_sorted += keys;
    });
}

/// The operation ended: add its `keysSorted` and `sortTotalDataSizeBytes`
/// to `metrics.query.sort`, as mongod 8.2.11 does for each operation
/// (`curop_metrics.cpp`) -- each `getMore` of a cursor with what its plan
/// has sorted so far, so a sort is counted again at every one (RM-400).
pub fn count_sorted() {
    REPORT.with(|r| {
        let r = r.borrow();
        SORT_METRICS.total_keys_sorted.add(r.keys_sorted);
        SORT_METRICS.total_bytes_sorted.add(r.sort_total_data_size);
    });
}

/// Add what a cursor's earlier operations reported of its plan to this
/// operation's: mongod's every operation on a cursor reports its plan's
/// sort and group counts so far (RM-400).
pub fn add_report(earlier: &SpillReport) {
    REPORT.with(|r| {
        let mut r = r.borrow_mut();
        r.used_disk |= earlier.used_disk;
        r.has_sort_stage |= earlier.has_sort_stage;
        r.group_spills += earlier.group_spills;
        r.group_spilled_records += earlier.group_spilled_records;
        r.group_spilled_bytes += earlier.group_spilled_bytes;
        r.group_spilled_data_storage_size += earlier.group_spilled_data_storage_size;
    });
    add_sort_counts(earlier, 1);
}

/// A sort wrote a run (a spill, or a merge of runs) of `records`, as
/// `chunks` chunks of its file.
pub(crate) fn record_sort_spill(records: i64, bytes: i64, storage: i64, chunks: i64) {
    REPORT.with(|r| {
        let mut r = r.borrow_mut();
        r.used_disk = true;
        r.sort_spills += 1;
        r.sort_spilled_records += records;
        r.sort_spilled_bytes += bytes;
        r.sort_spilled_data_storage_size += storage;
    });
    SORT_METRICS.spill_to_disk.add(chunks);
    SORT_METRICS.spill_to_disk_bytes.add(storage);
}

/// `$bucketAuto`'s sorter wrote a run: counted as the stage's own, and its
/// chunks as every sorter's (mongod 8.2.11: `metrics.query.sort` counts
/// what the sorter's file writer writes, whichever stage it sorts for).
pub(crate) fn record_bucket_auto_spill(records: i64, bytes: i64, storage: i64, chunks: i64) {
    REPORT.with(|r| r.borrow_mut().used_disk = true);
    BUCKET_AUTO_METRICS.add(1, records, bytes, storage);
    SORT_METRICS.spill_to_disk.add(chunks);
    SORT_METRICS.spill_to_disk_bytes.add(storage);
}

/// `$setWindowFields` spilled its partition cache.
pub(crate) fn record_window_spill(records: i64, bytes: i64, storage: i64) {
    REPORT.with(|r| r.borrow_mut().used_disk = true);
    WINDOW_METRICS.add(1, records, bytes, storage);
}

/// Restores the previous `allowDiskUse` when dropped.
pub struct DiskUse(bool);

impl Drop for DiskUse {
    fn drop(&mut self) {
        ALLOW_DISK_USE.with(|a| a.set(self.0));
    }
}

/// Run what follows with the command's `allowDiskUse`.
pub fn allow_disk_use(allow: bool) -> DiskUse {
    DiskUse(ALLOW_DISK_USE.with(|a| a.replace(allow)))
}

/// Whether a stage over its budget may spill.
pub fn disk_allowed() -> bool {
    ALLOW_DISK_USE.with(Cell::get)
}

/// Forget what an earlier operation on this thread spilled.
pub fn reset_report() {
    REPORT.with(|r| *r.borrow_mut() = SpillReport::default());
    CURSOR_READ.with(|c| c.borrow_mut().take());
}

/// A query-layer sort, spilled or not, read by the stages after it through
/// mongod's cursor stage (RM-326). mongod 8.2.11's cursor stage adds its
/// executor's summary to its own at every buffer it loads, and never resets
/// the sort's counts there, so the operation reports them once per load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorRead {
    /// What the sort counted, once.
    pub sort: SpillReport,
    /// The size of each document the executor hands over, in order.
    pub sizes: Vec<usize>,
    /// The stage of the pipeline the cursor stage feeds.
    pub at: usize,
    /// The query layer took a `$limit` after the sort.
    pub limited: bool,
    /// For each stage from `at` on whose reads depend on its documents (a
    /// `$match`, an `$unwind`, a `$setWindowFields`), keyed by its place
    /// after `at`: how many documents of its input it had read when it gave
    /// each of its outputs -- one past its input when it looked for the
    /// end. mongod pulls a document at a time through them, so these are
    /// what the cursor stage must load for a batch (RM-326).
    pub reads: std::collections::BTreeMap<usize, Vec<usize>>,
}

pub(crate) fn note_cursor_read(read: CursorRead) {
    CURSOR_READ.with(|c| *c.borrow_mut() = Some(read));
}

/// Whether the stage at `index` of the pipeline running reads documents a
/// query-layer sort hands on, so its reads are kept (RM-326).
pub(crate) fn tracks_reads(index: usize) -> bool {
    CURSOR_READ.with(|c| c.borrow().as_ref().is_some_and(|read| index >= read.at))
}

/// Keep what the stage at `index` read for each of its outputs.
pub(crate) fn note_stage_reads(index: usize, reads: Vec<usize>) {
    CURSOR_READ.with(|c| {
        if let Some(read) = c.borrow_mut().as_mut().filter(|read| index >= read.at) {
            read.reads.insert(index - read.at, reads);
        }
    });
}

/// The query-layer sort of the pipeline just run, when it spilled.
pub fn take_cursor_read() -> Option<CursorRead> {
    CURSOR_READ.with(|c| c.borrow_mut().take())
}

/// Count `sort`'s counts `times` more in the operation's report.
pub fn add_sort_counts(sort: &SpillReport, times: i64) {
    REPORT.with(|r| {
        let mut r = r.borrow_mut();
        r.sort_spills += sort.sort_spills * times;
        r.sort_spilled_records += sort.sort_spilled_records * times;
        r.sort_spilled_bytes += sort.sort_spilled_bytes * times;
        r.sort_spilled_data_storage_size += sort.sort_spilled_data_storage_size * times;
        r.sort_total_data_size += sort.sort_total_data_size * times;
        r.keys_sorted += sort.keys_sorted * times;
    });
}

/// What the operation has spilled so far.
pub fn report() -> SpillReport {
    REPORT.with(|r| r.borrow().clone())
}

/// Run `f` with a report of its own, and hand that back: what an explain
/// reports for one stage, kept out of the operation's own report.
pub fn captured<T>(f: impl FnOnce() -> T) -> (T, SpillReport) {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            STAGE_STATS.with(|s| s.set(self.0));
        }
    }
    let outer = REPORT.with(|r| std::mem::take(&mut *r.borrow_mut()));
    let _restore = Restore(STAGE_STATS.with(|s| s.replace(true)));
    let out = f();
    let inner = REPORT.with(|r| std::mem::replace(&mut *r.borrow_mut(), outer));
    (out, inner)
}

/// A `$group` finished (RM-328).
pub(crate) fn record_group(stats: impl FnOnce() -> GroupStats) {
    if STAGE_STATS.with(Cell::get) {
        REPORT.with(|r| r.borrow_mut().groups.push(stats()));
    }
}

/// A `$setWindowFields` finished (RM-328).
pub(crate) fn record_window(stats: impl FnOnce() -> WindowStats) {
    if STAGE_STATS.with(Cell::get) {
        REPORT.with(|r| r.borrow_mut().windows.push(stats()));
    }
}

pub(crate) fn record_group_spill(records: i64, bytes: i64, storage: i64) {
    REPORT.with(|r| {
        let mut r = r.borrow_mut();
        r.used_disk = true;
        r.group_spills += 1;
        r.group_spilled_records += records;
        r.group_spilled_bytes += bytes;
        r.group_spilled_data_storage_size += storage;
    });
    GROUP_METRICS.add(1, records, bytes, storage);
}

/// Where spill files go (mongod: `<dbpath>/_tmp`). Unset: the system's
/// temporary directory.
pub fn set_spill_dir(dir: Option<PathBuf>) {
    SPILL_DIR.with(|d| *d.borrow_mut() = dir);
}

pub(crate) fn spill_dir() -> PathBuf {
    SPILL_DIR
        .with(|d| d.borrow().clone())
        .unwrap_or_else(std::env::temp_dir)
}

/// The BSON size of an index key holding `values`: an object whose fields
/// have empty names, as mongod's index key data is.
pub fn index_key_len(values: &[Bson]) -> usize {
    5 + values.iter().map(|v| 2 + value_len(v)).sum::<usize>()
}

/// The BSON size of `doc`, without encoding it.
pub fn bson_len(doc: &Document) -> usize {
    5 + doc
        .iter()
        .map(|(k, v)| 2 + k.len() + value_len(v))
        .sum::<usize>()
}

fn array_len(items: &[Bson]) -> usize {
    5 + items
        .iter()
        .enumerate()
        .map(|(i, v)| 2 + digits(i) + value_len(v))
        .sum::<usize>()
}

fn digits(mut i: usize) -> usize {
    let mut n = 1;
    while i >= 10 {
        i /= 10;
        n += 1;
    }
    n
}

/// The bytes `v` takes as a BSON element's value.
fn value_len(v: &Bson) -> usize {
    match v {
        Bson::Double(_) | Bson::DateTime(_) | Bson::Timestamp(_) | Bson::Int64(_) => 8,
        Bson::String(s) | Bson::JavaScriptCode(s) | Bson::Symbol(s) => 5 + s.len(),
        Bson::Document(d) => bson_len(d),
        Bson::Array(a) => array_len(a),
        Bson::Binary(b) => {
            let old = u8::from(b.subtype) == 2;
            5 + b.bytes.len() + if old { 4 } else { 0 }
        }
        Bson::ObjectId(_) => 12,
        Bson::Boolean(_) => 1,
        Bson::RegularExpression(r) => r.pattern.len() + r.options.len() + 2,
        Bson::JavaScriptCodeWithScope(c) => 4 + 5 + c.code.len() + bson_len(&c.scope),
        Bson::Int32(_) => 4,
        Bson::Decimal128(_) => 16,
        // A DBPointer: a string namespace and an ObjectId.
        Bson::DbPointer(_) => 5 + 12 + 32,
        Bson::Null | Bson::Undefined | Bson::MinKey | Bson::MaxKey => 0,
    }
}

/// mongod's size of a document (`Document::getApproximateSize`).
pub fn document_size(doc: &Document) -> usize {
    bson_len(doc) + DOCUMENT_OVERHEAD
}

/// The bytes the top-level field `name` holding `v` takes in its document's
/// BSON (`BSONElement::size`).
pub fn element_len(name: &str, v: &Bson) -> usize {
    2 + name.len() + value_len(v)
}

/// mongod's size of a document the classic engine built field by field,
/// reserving room for its fields first (`MutableDocument(n)`), as a
/// `$group` builds each document it returns (`Document::getApproximateSize`;
/// measured on mongod 8.2.1, 2026-11-08, from `explain`'s
/// `totalOutputDataSizeBytes` over 1 to 14 fields of names 1 to 100 bytes
/// long, RM-328): its storage over an empty BSON object, the buffer
/// `DocumentStorage` allocated, and what each value holds beyond its own
/// `Value` ([`crate::docstore`]).
pub fn built_document_size(doc: &Document) -> usize {
    built_document_size_with(doc, |_, v| crate::docstore::held(v))
}

/// [`built_document_size`], `held` giving what each field's value holds
/// beyond its `Value` -- for a value an expression built rather than read.
pub fn built_document_size_with(doc: &Document, held: impl Fn(&str, &Bson) -> usize) -> usize {
    let mut s = crate::docstore::Stored::reserved(doc.len());
    for (name, v) in doc {
        s.add_field(name, held(name, v));
    }
    s.approximate()
}

/// mongod's size of `doc` cut down to the top-level `fields` a stage and
/// the stages after it read (what its dependency analysis keeps).
pub fn fields_size(doc: &Document, fields: &std::collections::BTreeSet<String>) -> usize {
    5 + doc
        .iter()
        .filter(|(k, _)| fields.contains(k.as_str()))
        .map(|(k, v)| 2 + k.len() + value_len(v))
        .sum::<usize>()
        + DOCUMENT_OVERHEAD
}

fn classic_string(len: usize) -> usize {
    if len <= SHORT_STR {
        VALUE
    } else {
        VALUE + RC_STRING + len
    }
}

/// The classic engine's size of a value (`Value::getApproximateSize`).
pub fn classic_size(v: &Bson) -> usize {
    match v {
        Bson::String(s) | Bson::JavaScriptCode(s) | Bson::Symbol(s) => classic_string(s.len()),
        Bson::RegularExpression(r) => classic_string(r.pattern.len() + 1 + r.options.len()),
        Bson::Binary(b) => classic_string(b.bytes.len()),
        Bson::Document(d) => VALUE + document_size(d),
        Bson::Array(a) => VALUE + RC_VECTOR + a.iter().map(classic_size).sum::<usize>(),
        Bson::Decimal128(_) => VALUE + RC_DECIMAL,
        Bson::JavaScriptCodeWithScope(c) => VALUE + RC_STRING + c.code.len() + bson_len(&c.scope),
        _ => VALUE,
    }
}

fn sbe_string(len: usize) -> usize {
    if len <= SBE_SMALL_STR {
        SBE_VALUE
    } else {
        SBE_VALUE + 4 + len + 1
    }
}

/// SBE's size of a value (`value::getApproximateSize`; measured 8.2.11: a
/// string of n bytes is n + 14, a document of n BSON bytes n + 9).
pub fn sbe_size(v: &Bson) -> usize {
    match v {
        Bson::String(s) | Bson::JavaScriptCode(s) | Bson::Symbol(s) => sbe_string(s.len()),
        Bson::Document(d) => SBE_VALUE + bson_len(d),
        Bson::Array(a) => SBE_VALUE + array_len(a),
        Bson::Binary(b) => SBE_VALUE + 4 + 1 + b.bytes.len(),
        Bson::ObjectId(_) => SBE_VALUE + 12,
        Bson::Decimal128(_) => SBE_VALUE + 16,
        Bson::RegularExpression(r) => SBE_VALUE + 4 + r.pattern.len() + r.options.len() + 2,
        Bson::JavaScriptCodeWithScope(_) => SBE_VALUE + value_len(v),
        _ => SBE_VALUE,
    }
}

/// The size of `v` in the engine the stage is running in.
pub fn engine_size(v: &Bson) -> usize {
    if crate::sbe::active() {
        sbe_size(v)
    } else {
        classic_size(v)
    }
}

/// One more document in a `$facet`'s output (`DocumentSourceFacet::doGetNext`):
/// refused (4031700) once the document being built passes 100 MiB.
pub fn facet_add(used: &mut usize, doc: &Document) -> Result<(), crate::fallback::Fallback> {
    let max = budgets().facet;
    *used += document_size(doc);
    if *used > max {
        return Err(crate::fallback::Fallback::mongo(
            4031700,
            format!(
                "document constructed by $facet is {used} bytes, which exceeds the limit of \
                 {max} bytes"
            ),
        ));
    }
    Ok(())
}

/// One more document in a `$lookup`'s result. The classic stage
/// (`DocumentSourceLookUp`) checks the total BEFORE this document is added
/// (4568), so the document that crosses 100 MiB is still taken and only the
/// next one is refused; SBE's join (`addToArrayCapped`) refuses the document
/// that would reach the limit, as a `$push` does (146). Measured 8.2.11,
/// 2026-10-26: 105 one-megabyte documents join, 106 do not.
pub fn lookup_add(
    used: &mut usize,
    count: usize,
    doc: &Document,
    from: &str,
) -> Result<(), crate::fallback::Fallback> {
    let max = budgets().lookup;
    if crate::sbe::active() {
        let elem = SBE_VALUE + bson_len(doc);
        if *used + elem >= max {
            return Err(crate::fallback::Fallback::mongo(
                146,
                format!(
                    "Used too much memory for a single array. Memory limit: {max} bytes. \
                     The array contains {count} elements and is of size {used} bytes. The \
                     element being added has size {elem} bytes."
                ),
            ));
        }
        *used += elem;
        return Ok(());
    }
    if *used > max {
        return Err(crate::fallback::Fallback::mongo(
            4568,
            format!(
                "Total size of documents in {from} matching pipeline's $lookup stage exceeds \
                 {max} bytes"
            ),
        ));
    }
    *used += document_size(doc);
    Ok(())
}

/// `{_id: <int>, s: <1,000,000 x's>}`, the document every error number
/// above was measured on.
#[cfg(test)]
pub(crate) fn measured_doc(id: i32, len: usize) -> Document {
    bson::doc! { "_id": id, "s": "x".repeat(len) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn bson_len_is_the_encoded_size() {
        let docs = [
            doc! {},
            doc! { "a": 1, "b": "xyz", "c": [1, "two", { "d": 3.5 }], "e": null },
            doc! { "big": "x".repeat(1000), "arr": (0..20).map(Bson::Int64).collect::<Vec<_>>() },
            doc! { "o": bson::oid::ObjectId::new(), "t": true, "dec": Bson::Decimal128(bson::Decimal128::from_bytes([0; 16])) },
        ];
        for d in docs {
            assert_eq!(bson_len(&d), bson::to_vec(&d).unwrap().len(), "{d:?}");
        }
    }

    #[test]
    fn a_built_document_is_sized_as_mongod_sizes_one() {
        // mongod 8.2.1's `totalOutputDataSizeBytes` per `$group` document
        // (2026-11-08): `_id` and `n` int fields named `name(i)`.
        let built = |fields: usize, name: &dyn Fn(usize) -> String| {
            let mut d = doc! { "_id": 1 };
            for i in 0..fields {
                d.insert(name(i), 1);
            }
            built_document_size(&d)
        };
        let short = |i: usize| ((b'a' + i as u8) as char).to_string();
        let ones: Vec<usize> = (0..14).map(|n| built(n, &short)).collect();
        assert_eq!(
            ones,
            [205, 237, 269, 301, 333, 365, 397, 429, 493, 525, 557, 589, 621, 653]
        );
        for (len, size) in [(1, 237), (20, 237), (36, 237), (40, 365), (100, 365)] {
            assert_eq!(built(1, &|_| "a".repeat(len)), size, "a name of {len}");
        }
        let long = |len: usize| move |i: usize| short(i).repeat(len);
        let twenties: Vec<usize> = (0..9).map(|n| built(n, &long(20))).collect();
        assert_eq!(twenties, [205, 237, 269, 365, 365, 621, 621, 621, 621]);
        let thirties: Vec<usize> = (0..9).map(|n| built(n, &long(30))).collect();
        assert_eq!(thirties, [205, 237, 365, 365, 621, 621, 621, 621, 1133]);
        // What a value holds beyond its `Value`: a 1000-byte string 1024.
        assert_eq!(
            built_document_size(&doc! { "_id": 1, "f": "x".repeat(1000) }),
            237 + 1024
        );
    }

    #[test]
    fn a_group_reports_its_accumulators_and_output() {
        // mongod 8.2.1 over 300 documents in 7 groups (2026-11-08): the
        // documents it builds take 1659 bytes; each `$sum` is counted over
        // every group at once, at the build's size of a `$sum` (176 bytes on
        // Linux, 184 on macOS arm64).
        let docs: Vec<Document> = (0..300).map(|i| doc! {"_id": i, "k": i % 7}).collect();
        let spec = Bson::Document(doc! {"_id": "$k", "n": {"$sum": 1}});
        let (out, report) =
            captured(|| crate::group::group_stage(&spec, &docs, &Document::new()).unwrap());
        assert_eq!(out.len(), 7);
        // Kept only while explain reads them.
        reset_report();
        crate::group::group_stage(&spec, &docs, &Document::new()).unwrap();
        assert!(super::report().groups.is_empty());
        assert_eq!(
            report.groups,
            [GroupStats {
                max_accumulator_memory: vec![("n".to_string(), 7 * LAYOUT.sum as i64)],
                total_output_data_size: 1659,
            }]
        );
    }

    #[test]
    fn sizes_match_mongods_error_counts() {
        // mongod 8.2.11 over `{_id: i, s: 1,000,000 x's}`: 1,000,126 per
        // document in `$facet` and `$setWindowFields`.
        assert_eq!(document_size(&measured_doc(1, 1_000_000)), 1_000_126);
        // SBE `$push` of the string: 1,000,014; of the document: 1,000,031.
        assert_eq!(sbe_size(&Bson::String("x".repeat(1_000_000))), 1_000_014);
        assert_eq!(
            sbe_size(&Bson::Document(measured_doc(1, 1_000_000))),
            1_000_031
        );
        // Classic `$addToSet` of 105 strings of 1,000,001-1,000,003 bytes
        // reports 105,004,549 used, its own 144 bytes included.
        let used: usize = ACCUMULATOR_BASE
            + (0..105)
                .map(|i| classic_size(&Bson::String(format!("{}{i}", "x".repeat(1_000_000)))))
                .sum::<usize>();
        assert_eq!(used, 105_004_549);
    }

    #[test]
    fn allow_disk_use_is_scoped() {
        assert!(disk_allowed());
        {
            let _off = allow_disk_use(false);
            assert!(!disk_allowed());
        }
        assert!(disk_allowed());
    }
}
