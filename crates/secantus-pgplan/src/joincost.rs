//! PostgreSQL 15's join costing (`initial_cost_hashjoin` /
//! `final_cost_hashjoin`, `initial_cost_mergejoin` / `final_cost_mergejoin`,
//! `eqjoinsel`, `compute_semi_anti_join_factors`,
//! `estimate_hash_bucket_stats`, `ExecChooseHashTableSize`) for a join of two
//! tables on one equality, over tables with no statistics (RP-028).
//!
//! What it decides: whether the plan of a join under an ORDER BY of a join
//! key is a merge join already in that order, or a Sort on top of the
//! cheapest join -- which is whether a cursor over it can scan backward
//! (`ExecSupportsBackwardScan`: a Sort can, a join cannot). Each formula is
//! checked against `EXPLAIN` on PostgreSQL 15.19 in the tests below.

use crate::costsize::clamp_row_est;

const CPU_TUPLE_COST: f64 = 0.01;
const CPU_OPERATOR_COST: f64 = 0.0025;

/// One side of the join, as the planner sees it.
#[derive(Debug, Clone, Copy)]
pub struct JoinSide {
    /// Its estimated rows (after its own quals) and the table's tuples.
    pub rows: f64,
    pub tuples: f64,
    /// The cheapest scan: `(startup, total)`.
    pub scan: (f64, f64),
    /// The cheapest scan to start, a sequential scan: `(startup, total)`.
    /// Kept beside `scan` under a LIMIT (`consider_startup`).
    pub startup_scan: (f64, f64),
    /// An index scan in the join key's order, if an index leads with it
    /// (for a join's output, its cheapest path already in that order).
    pub ordered: Option<(f64, f64)>,
    /// The cheapest scan (`scan`) is itself in the key's order: a join's
    /// output whose cheapest path is a merge join on that key, which a
    /// merge join above reads without a Sort.
    pub scan_ordered: bool,
    /// The join key is unique (a one-column primary key or unique index).
    pub unique: bool,
    /// The join key's distinct count (`get_variable_numdistinct`), and
    /// whether it is the 200 default.
    pub ndistinct: f64,
    pub default_ndistinct: bool,
    /// The operators its side of the join clause evaluates: 0 for a column,
    /// 1 for `c + 3`.
    pub key_ops: f64,
    /// The rows of the table the key's column is in, which a hash bucket's
    /// size scales its distinct count by (`estimate_hash_bucket_stats`):
    /// `rows` for a table, the base table's for a join's output.
    pub rel_rows: f64,
    /// The width of its rows (`pathtarget->width`): the columns needed
    /// above it. `None` where it is not known, which prices a hash join on
    /// it as one batch, as before widths were modelled.
    pub width: Option<i64>,
    /// The join's further clauses' keys on this side, in clause order
    /// (`a.x = b.y AND a.z = b.w`: z here when this is `a`); none for a
    /// join on one clause.
    pub extra: ExtraKeys,
}

/// Up to three more join keys of a side, each its distinct count, whether
/// that is the 200 default, and whether the key is unique on its own.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ExtraKeys {
    pub n: usize,
    pub ndistinct: [f64; 3],
    pub default_ndistinct: [bool; 3],
    pub unique: [bool; 3],
}

impl ExtraKeys {
    /// The keys after the first: `(ndistinct, default, unique)` each.
    pub fn of(keys: &[(f64, bool, bool)]) -> Option<ExtraKeys> {
        if keys.len() > 3 {
            return None;
        }
        let mut e = ExtraKeys {
            n: keys.len(),
            ..ExtraKeys::default()
        };
        for (i, &(nd, default, unique)) in keys.iter().enumerate() {
            e.ndistinct[i] = nd;
            e.default_ndistinct[i] = default;
            e.unique[i] = unique;
        }
        Some(e)
    }
}

impl JoinSide {
    /// The planner's numbers for a column of a never-analysed table: the
    /// tuples when unique, else 200 (`DEFAULT_NUM_DISTINCT`), or the tuples
    /// when fewer.
    pub fn ndistinct_for(unique: bool, tuples: f64) -> (f64, bool) {
        if unique || tuples < 200.0 {
            (tuples, false)
        } else {
            (200.0, true)
        }
    }
}

/// `cost_sort` over `rows` rows of an input costing `input_total`, in memory:
/// `(startup, total)`.
fn sort_cost(input_total: f64, rows: f64) -> (f64, f64) {
    let n = rows.max(2.0);
    let startup = input_total + 2.0 * CPU_OPERATOR_COST * n * n.log2();
    (startup, startup + CPU_OPERATOR_COST * rows)
}

/// `tuplesort_merge_order` for `work_mem`: the tapes a merge pass reads,
/// a tape's buffer and a merge buffer (32 blocks) each, at least 6.
fn tuplesort_merge_order() -> f64 {
    let order = (WORK_MEM_BYTES / (2.0 * 8192.0 + 32.0 * 8192.0)).floor();
    order.clamp(6.0, 500.0)
}

/// `cost_sort` (`cost_tuplesort`) over `tuples` rows `width` bytes wide of
/// an input costing `input_total`, of which a LIMIT reads `limit` (`None`
/// for all): an external merge sort when what it keeps outgrows
/// `work_mem` (N log2 N comparisons and its runs' pages, three quarters
/// read in order), a bounded heap when the LIMIT keeps less than half the
/// rows (N log2 2K), else a quicksort (N log2 N); and an operator a row it
/// returns. `(startup, total)`.
pub fn cost_sort(input_total: f64, tuples: f64, width: i64, limit: Option<f64>) -> (f64, f64) {
    let input_bytes = relation_byte_size(tuples, width);
    let n = tuples.max(2.0);
    let comparison = 2.0 * CPU_OPERATOR_COST;
    let (output_tuples, output_bytes) = match limit {
        Some(l) if l > 0.0 && l < n => (l, relation_byte_size(l, width)),
        _ => (n, input_bytes),
    };
    let startup = if output_bytes > WORK_MEM_BYTES {
        let npages = (input_bytes / 8192.0).ceil();
        let nruns = input_bytes / WORK_MEM_BYTES;
        let order = tuplesort_merge_order();
        let log_runs = if nruns > order {
            (nruns.ln() / order.ln()).ceil()
        } else {
            1.0
        };
        let accesses = 2.0 * npages * log_runs;
        comparison * n * n.log2() + accesses * (0.75 + RANDOM_PAGE_COST * 0.25)
    } else if n > 2.0 * output_tuples || input_bytes > WORK_MEM_BYTES {
        comparison * n * (2.0 * output_tuples).log2()
    } else {
        comparison * n * n.log2()
    };
    let startup = input_total + startup;
    (startup, startup + CPU_OPERATOR_COST * n)
}

/// `random_page_cost`.
const RANDOM_PAGE_COST: f64 = 4.0;

/// `initial_cost_mergejoin` / `final_cost_mergejoin` over whole pathlists
/// (`JoinRel::add_paths_general`): each side read presorted
/// (`*_by_index`) or through a `cost_sort` of its path (an external sort
/// when it outgrows `work_mem`, `width` known); the inner side's
/// rescanned tuples, none when it is unique and its clause the join's
/// only qual (`skip_mark_restore`); the inner materialised when that is
/// cheaper, when it is presorted by a path that cannot mark and restore
/// (`ExecSupportsMarkRestore`: only an index scan here; `inner_markable`),
/// or when its Sort outgrows `work_mem`. Join filters costing `qp` a tuple
/// on each tuple the merge clause passes.
fn merge_join_full(
    outer: &JoinSide,
    inner: &JoinSide,
    outer_by_index: bool,
    inner_by_index: bool,
    qp: f64,
    inner_markable: bool,
) -> Option<(f64, f64)> {
    let input = |side: &JoinSide, by_index: bool| -> Option<(f64, f64)> {
        if by_index {
            side.ordered
        } else {
            Some(cost_sort(side.scan.1, side.rows, side.width?, None))
        }
    };
    let (os, ot) = input(outer, outer_by_index)?;
    let (is, it) = input(inner, inner_by_index)?;
    let startup = os + is;
    let mut run = ot - os;
    let inner_run = it - is;
    let tuples = clamp_row_est(outer.rows * inner.rows * join_selectivity(outer, inner));
    let skip_mark_restore = inner.unique && qp == 0.0;
    let rescanned = if skip_mark_restore {
        0.0
    } else {
        (tuples - inner.rows).max(0.0)
    };
    let ratio = 1.0 + rescanned / inner.rows.max(1.0);
    let bare = inner_run * ratio;
    let materialized = inner_run + CPU_OPERATOR_COST * inner.rows * ratio;
    let materialize = !skip_mark_restore
        && (materialized < bare
            || (inner_by_index && !inner_markable)
            || (!inner_by_index && relation_byte_size(inner.rows, inner.width?) > WORK_MEM_BYTES));
    run += if materialize { materialized } else { bare };
    run += clause_cost(outer, inner) * (outer.rows + inner.rows * ratio);
    run += (CPU_TUPLE_COST + qp) * tuples;
    Some((startup, startup + run))
}

/// The join clause's cost a tuple (`cost_qual_eval`): its `=` and each
/// side's own operators.
fn clause_cost(outer: &JoinSide, inner: &JoinSide) -> f64 {
    CPU_OPERATOR_COST * (1.0 + outer.extra.n as f64 + outer.key_ops + inner.key_ops)
}

/// `eqjoinsel` with no statistics: `1 / max(nd1, nd2)`, a clause each, the
/// clauses' selectivities multiplied (`clauselist_selectivity`). A plain
/// or LEFT join's semi-join factors use this too (the special join info's
/// type is not SEMI).
fn join_selectivity(outer: &JoinSide, inner: &JoinSide) -> f64 {
    let mut sel = 1.0 / outer.ndistinct.max(inner.ndistinct).max(1.0);
    for i in 0..outer.extra.n.min(inner.extra.n) {
        sel *= 1.0
            / outer.extra.ndistinct[i]
                .max(inner.extra.ndistinct[i])
                .max(1.0);
    }
    sel
}

/// The join's estimated rows (`calc_joinrel_size_estimate`); a LEFT join
/// keeps at least every outer row.
pub fn join_rows(outer: &JoinSide, inner: &JoinSide, left: bool) -> f64 {
    let n = outer.rows * inner.rows * join_selectivity(outer, inner);
    clamp_row_est(if left { n.max(outer.rows) } else { n })
}

/// A FULL join's estimated rows (`calc_joinrel_size_estimate`): at least
/// every row of either side.
pub fn full_join_rows(a: &JoinSide, b: &JoinSide) -> f64 {
    let n = a.rows * b.rows * join_selectivity(a, b);
    clamp_row_est(n.max(a.rows).max(b.rows))
}

/// `ExecChooseHashTableSize`'s buckets for a hashed side estimated at
/// `rows`: a power of two at least the rows, at least 1,024.
pub fn hash_buckets(rows: f64) -> usize {
    (rows.max(1.0).ceil() as usize)
        .max(1024)
        .next_power_of_two()
}

/// The bucket of a hash join's table a key `v` of type `ty` lands in
/// (`ExecHashGetHashValue`, `ExecHashGetBucketAndBatch`): the type's hash
/// function's value, masked; a NULL a right join keeps hashes as 0. `None`
/// for a type whose hash is not modelled.
pub fn hash_bucket(v: &bson::Bson, ty: &str, nbuckets: usize) -> Option<usize> {
    let hash = match v {
        bson::Bson::Null => 0,
        v => crate::hashfn::plain_hash(v, ty)?,
    };
    Some(hash as usize & (nbuckets - 1))
}

/// `work_mem` x `hash_mem_multiplier` (`get_hash_memory_limit`), at
/// PostgreSQL 15's defaults of 4 MB and 2.0.
const HASH_MEM_BYTES: u64 = 4096 * 1024 * 2;

fn maxalign(n: u64) -> u64 {
    (n + 7) & !7
}

/// `ExecChooseHashTableSize` for a hashed side of `rows` rows `width` bytes
/// wide, its skew allowance counted (the planner always counts it, and the
/// executor does for an outer key that is a table's column, which a chain's
/// always is): `(nbuckets, nbatch)`.
pub fn hash_table_size(rows: f64, width: i64) -> (u64, u64) {
    // HJTUPLE_OVERHEAD (16), MAXALIGN(SizeofMinimalTupleHeader) (16).
    let ntuples = if rows <= 0.0 { 1000.0 } else { rows };
    let tupsize = 16 + 16 + maxalign(width.max(0) as u64);
    let inner_rel_bytes = ntuples * tupsize as f64;
    let mut hash_table_bytes = HASH_MEM_BYTES;
    // The skew table: 2% of the memory, at a tuple, eight bucket pointers,
    // an int and a HashSkewBucket (16) per most common value.
    let bytes_per_mcv = tupsize + 8 * 8 + 4 + 16;
    let skew_mcvs = (hash_table_bytes / bytes_per_mcv) * 2 / 100;
    hash_table_bytes -= skew_mcvs * bytes_per_mcv;
    // A HashJoinTuple pointer a bucket; MaxAllocSize / 8 at most.
    let max_pointers = prev_power_of_two((hash_table_bytes / 8).min(0x3fff_ffff / 8));
    let max_pointers = max_pointers.min(i32::MAX as u64 / 2 + 1);
    let dbuckets = ntuples.ceil().min(max_pointers as f64);
    let mut nbuckets = (dbuckets as u64).max(1024).next_power_of_two();
    let mut nbatch = 1;
    if inner_rel_bytes + (8 * nbuckets) as f64 > hash_table_bytes as f64 {
        let bucket_size = tupsize + 8;
        let sbuckets = if hash_table_bytes <= bucket_size {
            1
        } else {
            (hash_table_bytes / bucket_size).next_power_of_two()
        };
        nbuckets = sbuckets.min(max_pointers).next_power_of_two();
        let bucket_bytes = nbuckets * 8;
        let dbatch = (inner_rel_bytes / (hash_table_bytes - bucket_bytes) as f64)
            .ceil()
            .min(max_pointers as f64);
        nbatch = (dbatch as u64).max(2).next_power_of_two();
    }
    (nbuckets, nbatch)
}

fn prev_power_of_two(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        1 << (63 - n.leading_zeros())
    }
}

/// A join key's hash (`ExecHashGetHashValue` over one key): the type's hash
/// function's value; `None` for a type whose hash is not modelled.
pub fn hash_value(v: &bson::Bson, ty: &str) -> Option<u32> {
    crate::hashfn::plain_hash(v, ty)
}

/// The batch a row whose key hashes to `hash` joins in
/// (`ExecHashGetBucketAndBatch`): its hash rotated right by log2(nbuckets),
/// masked by nbatch - 1.
pub fn hash_batch(hash: u32, nbuckets: u64, nbatch: u64) -> u64 {
    if nbatch <= 1 {
        return 0;
    }
    u64::from(hash.rotate_right(nbuckets.trailing_zeros())) & (nbatch - 1)
}

/// `page_size`: the 8 kB pages `rows` rows of `width` bytes take, at a
/// heap tuple header (23, aligned to 24) each.
fn page_size(rows: f64, width: i64) -> f64 {
    (rows * (maxalign(width.max(0) as u64) + 24) as f64 / 8192.0).ceil()
}

/// A hash join with `inner` hashed: `(startup, total)`.
pub fn hash_join(outer: &JoinSide, inner: &JoinSide) -> (f64, f64) {
    hash_join_qp(outer, inner, 0.0)
}

/// `hash_join` evaluating join filters costing `qp` a tuple on each tuple
/// the hash clause passes (`final_cost_hashjoin`'s `qp_qual_cost` over
/// `hashjointuples`).
fn hash_join_qp(outer: &JoinSide, inner: &JoinSide, qp: f64) -> (f64, f64) {
    let mut startup = outer.scan.0 + inner.scan.1;
    let mut run = outer.scan.1 - outer.scan.0;
    startup += (CPU_OPERATOR_COST + CPU_TUPLE_COST) * inner.rows;
    run += CPU_OPERATOR_COST * outer.rows;
    // ExecChooseHashTableSize; a side whose width is not known is priced
    // as one batch of a power of two at least the rows, at least 1,024.
    let (nbuckets, nbatch) = match inner.width {
        Some(w) => hash_table_size(inner.rows, w),
        None => (
            (inner.rows.max(1.0).ceil() as u64)
                .next_power_of_two()
                .max(1024),
            1,
        ),
    };
    // Batches write the inner side out once and the outer side out and
    // back (`initial_cost_hashjoin`).
    if nbatch > 1 {
        let innerpages = page_size(inner.rows, inner.width.unwrap_or(0));
        let outerpages = page_size(outer.rows, outer.width.unwrap_or(0));
        startup += innerpages;
        run += innerpages + 2.0 * outerpages;
    }
    // `final_cost_hashjoin`'s virtual buckets: every batch's.
    let buckets = nbuckets as f64 * nbatch as f64;
    // estimate_hash_bucket_stats: 0.1 for a default distinct count; of
    // several hash clauses the smallest bucket (`final_cost_hashjoin`).
    let bucket_of = |nd: f64, default: bool| {
        if default {
            return 0.1;
        }
        let mut nd = nd;
        if inner.tuples > 0.0 {
            nd = clamp_row_est(nd * inner.rel_rows / inner.tuples);
        }
        let f = if nd > buckets {
            1.0 / buckets
        } else {
            1.0 / nd
        };
        f.clamp(1.0e-6, 1.0)
    };
    let mut bucketsize = bucket_of(inner.ndistinct, inner.default_ndistinct);
    for i in 0..inner.extra.n {
        bucketsize = bucketsize.min(bucket_of(
            inner.extra.ndistinct[i],
            inner.extra.default_ndistinct[i],
        ));
    }
    let sel = join_selectivity(outer, inner);
    let tuples = if inner.unique {
        let match_frac = sel;
        let match_count = (sel * inner.rows / sel).max(1.0);
        let matched = (outer.rows * match_frac).round();
        let scan_frac = 2.0 / (match_count + 1.0);
        run += clause_cost(outer, inner)
            * matched
            * clamp_row_est(inner.rows * bucketsize * scan_frac)
            * 0.5;
        run += clause_cost(outer, inner)
            * (outer.rows - matched)
            * clamp_row_est(inner.rows / buckets)
            * 0.05;
        matched
    } else {
        run +=
            clause_cost(outer, inner) * outer.rows * clamp_row_est(inner.rows * bucketsize) * 0.5;
        clamp_row_est(outer.rows * inner.rows * sel)
    };
    run += (CPU_TUPLE_COST + qp) * tuples;
    (startup, startup + run)
}

/// A nested loop rescanning `inner` for each outer row, the inner read
/// whole each time or, when `materialized`, once into a Material node
/// (`cost_material`: two operator costs a tuple, in memory) and rescanned
/// at one operator cost a tuple (`cost_rescan`). `initial_cost_nestloop` /
/// `final_cost_nestloop`: the join clause is evaluated for every pair.
/// `(startup, total)`.
pub fn nest_loop(outer: &JoinSide, inner: &JoinSide, materialized: bool) -> (f64, f64) {
    nest_loop_with(outer, inner, materialized, clause_cost(outer, inner))
}

/// `nest_loop` evaluating join quals costing `qual` a pair: none for a
/// join whose only clause an equivalence class with a constant replaced
/// (`generate_base_implied_equalities_const`).
fn nest_loop_with(outer: &JoinSide, inner: &JoinSide, materialized: bool, qual: f64) -> (f64, f64) {
    let (inner_startup, inner_total, rescan) = if materialized {
        let total = inner.scan.1 + 2.0 * CPU_OPERATOR_COST * inner.rows;
        (inner.scan.0, total, CPU_OPERATOR_COST * inner.rows)
    } else {
        (inner.scan.0, inner.scan.1, inner.scan.1)
    };
    let startup = outer.scan.0 + inner_startup;
    let mut run = outer.scan.1 - outer.scan.0;
    run += inner_total - inner_startup;
    if outer.rows > 1.0 {
        run += (outer.rows - 1.0) * rescan;
    }
    run += (CPU_TUPLE_COST + qual) * outer.rows * inner.rows;
    (startup, startup + run)
}

/// A merge join, each side read in key order -- by its `ordered` index scan
/// when `by_index`, else sorted from its cheapest scan: `(startup, total)`.
/// No statistics: the merge reads both inputs whole (`mergejoinscansel`).
pub fn merge_join(
    outer: &JoinSide,
    inner: &JoinSide,
    outer_by_index: bool,
    inner_by_index: bool,
) -> Option<(f64, f64)> {
    merge_join_qp(outer, inner, outer_by_index, inner_by_index, 0.0)
}

/// `merge_join` evaluating join filters costing `qp` a tuple on each tuple
/// the merge clause passes (`final_cost_mergejoin`'s `qp_qual_cost` over
/// `mergejointuples`).
fn merge_join_qp(
    outer: &JoinSide,
    inner: &JoinSide,
    outer_by_index: bool,
    inner_by_index: bool,
    qp: f64,
) -> Option<(f64, f64)> {
    let input = |side: &JoinSide, by_index: bool| -> Option<(f64, f64)> {
        if by_index {
            side.ordered
        } else {
            Some(sort_cost(side.scan.1, side.rows))
        }
    };
    let (os, ot) = input(outer, outer_by_index)?;
    let (is, it) = input(inner, inner_by_index)?;
    let mut startup = os + is;
    let mut run = ot - os;
    let inner_run = it - is;
    let tuples = clamp_row_est(outer.rows * inner.rows * join_selectivity(outer, inner));
    // An inner side known unique skips mark and restore: nothing rescanned.
    let skip_mark_restore = inner.unique;
    let rescanned = if skip_mark_restore {
        0.0
    } else {
        (tuples - inner.rows).max(0.0)
    };
    let ratio = 1.0 + rescanned / inner.rows.max(1.0);
    let bare = inner_run * ratio;
    let materialized = inner_run + CPU_OPERATOR_COST * inner.rows * ratio;
    // An index scan supports mark and restore, a Sort too; the
    // materialised inner is chosen only when cheaper.
    run += if !skip_mark_restore && materialized < bare {
        materialized
    } else {
        bare
    };
    startup += 0.0;
    run += clause_cost(outer, inner) * (outer.rows + inner.rows * ratio);
    run += (CPU_TUPLE_COST + qp) * tuples;
    Some((startup, startup + run))
}

/// Whether a join of `a` and `b` on their keys, under an ORDER BY of the key
/// of `a` (or, for an inner join, either key -- they are one equivalence
/// class), is planned as a join already in that order rather than a Sort on
/// top. `left` is a LEFT join of `a` to `b`. `fraction` is the share of the
/// rows the plan is chosen for: a cursor's `cursor_tuple_fraction` (0.1),
/// so a plan is priced `startup + fraction * (total - startup)`
/// (`get_cheapest_fractional_path`); 1 for a whole result.
pub fn ordered_join_on_top(
    a: &JoinSide,
    b: &JoinSide,
    left: bool,
    order_on_a: bool,
    fraction: f64,
) -> bool {
    let rows = join_rows(a, b, left);
    let price = |(s, t): (f64, f64)| s + fraction * (t - s);
    // The cheapest join in total, which `create_ordered_paths` sorts, and
    // the cheapest already in the ORDER BY's order.
    let mut cheapest = (f64::INFINITY, f64::INFINITY);
    let mut ordered = f64::INFINITY;
    for (outer, inner, outer_is_a) in [(a, b, true), (b, a, false)] {
        let hash = hash_join(outer, inner);
        if hash.1 < cheapest.1 {
            cheapest = hash;
        }
        // `sort_inner_and_outer` sorts both sides; `match_unsorted_outer`
        // merges an outer already in order (an index scan) with the inner
        // sorted or read by its index. An explicitly sorted outer over an
        // inner index scan is never built.
        for (oi, ii) in [(false, false), (true, false), (true, true)] {
            let Some(merge) = merge_join(outer, inner, oi, ii) else {
                continue;
            };
            if merge.1 < cheapest.1 {
                cheapest = merge;
            }
            // A merge join's order is its outer key's: the ORDER BY's for
            // an inner join on either side; for a LEFT join only with `a`
            // outer and the ORDER BY on `a`'s key.
            if !left || (outer_is_a && order_on_a) {
                ordered = ordered.min(price(merge));
            }
        }
    }
    ordered <= price(sort_cost(cheapest.1, rows))
}

/// The plan PostgreSQL picks for a whole join of `a` (the left table) and
/// `b` on their keys, with no ORDER BY: the cheapest in total.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JoinMethod {
    /// A hash join probing with `a`'s rows in scan order, `b` hashed, and
    /// `b`'s key repeating (a unique one has one match a probe, in any
    /// order).
    HashOuterA,
    /// A merge join with `a` outer, each side read in key order -- by its
    /// index (`*_by_index`) or a Sort.
    MergeOuterA { a_by_index: bool, b_by_index: bool },
    /// An inner join with `b` outer: a hash join probing with `b`'s rows in
    /// scan order, `a` hashed.
    HashOuterB,
    /// An inner join with `b` outer: a merge join, each side in key order.
    MergeOuterB { a_by_index: bool, b_by_index: bool },
    /// An inner join with `b` outer: a nested loop, both sides in scan
    /// order.
    NestOuterB,
    /// A LEFT join with `b` outer: a hash right join probing with `b`'s
    /// rows in scan order, `a` hashed, and the `a` rows nothing matched
    /// last, in the hash table's order (`ExecScanHashTableForUnmatched`).
    HashRightB,
    /// A LEFT join with `b` outer: a merge right join, each side in key
    /// order, the `a` rows nothing matched in their key's place
    /// (`ExecMergeJoin`'s inner fill).
    MergeRightB { a_by_index: bool, b_by_index: bool },
    /// A FULL join: a hash full join probing with `a`'s rows in scan
    /// order, `b` hashed, each `a` row nothing matched in its place and the
    /// `b` rows nothing matched last, in the hash table's order.
    HashFullA,
    /// A FULL join: a hash full join probing with `b`'s rows, `a` hashed.
    HashFullB,
    /// A FULL join: a merge full join with `a` outer, each side in key
    /// order and the rows of either side nothing matched in their key's
    /// place (`ExecMergeJoin`'s outer and inner fill).
    MergeFullA { a_by_index: bool, b_by_index: bool },
    /// A FULL join: a merge full join with `b` outer.
    MergeFullB { a_by_index: bool, b_by_index: bool },
    /// A plan whose rows come in both sides' scan order, `a` outer: a
    /// nested loop, or a hash join on a unique `b`.
    Other,
}

/// One path `add_path` weighs: its `(startup, total)`, whether it comes
/// out in the join key's order (a merge join's pathkeys), and its plan.
#[derive(Debug, Clone, Copy)]
struct JoinPath {
    startup: f64,
    total: f64,
    keyed: bool,
    /// The equivalence class a keyed path is in the order of
    /// (`add_paths_on`); paths keyed on two classes have different
    /// pathkeys, which `add_path` never compares.
    class: usize,
    method: JoinMethod,
    combo: usize,
    /// The path of each input it reads (`RelPaths::paths`' index), or
    /// `DEFAULT_INPUT` where the input was a `JoinSide` alone.
    a_path: usize,
    b_path: usize,
    /// Its rows: the relation's, or fewer for a scan parameterized by an
    /// outer row (`ppi_rows`); 0 where every path of a relation is
    /// unparameterized and so has the same.
    rows: f64,
    /// The tables (a bit each) a parameterized path needs from an outer
    /// side (`PATH_REQ_OUTER`); 0 for none.
    req_outer: u32,
    /// What reading it again costs (`cost_rescan`): `(startup, total)`.
    rescan: (f64, f64),
}

/// `JoinChoice::a_path` / `b_path` for a path built from a `JoinSide`
/// alone (`JoinRel::add_paths_on`), whose input paths are its cheapest.
pub const DEFAULT_INPUT: usize = usize::MAX;

/// `compare_path_costs_fuzzily`: which of `p1` and `p2` is cheaper in
/// total by more than `fuzz` (or, fuzzily equal there, in startup); when a
/// path's startup cost counts (`CONSIDER_PATH_STARTUP_COST`: its relation
/// weighs it, and it is not parameterized), a path cheaper in total but
/// dearer to start is `Different`, and both are kept.
fn fuzzy_cmp(p1: &JoinPath, p2: &JoinPath, fuzz: f64, startup_counts: bool) -> Ordering3 {
    let counts = |p: &JoinPath| startup_counts && p.req_outer == 0;
    if p1.total > p2.total * fuzz {
        if counts(p1) && p2.startup > p1.startup * fuzz {
            return Ordering3::Different;
        }
        return Ordering3::Better2;
    }
    if p2.total > p1.total * fuzz {
        if counts(p2) && p1.startup > p2.startup * fuzz {
            return Ordering3::Different;
        }
        return Ordering3::Better1;
    }
    if p1.startup > p2.startup * fuzz {
        return Ordering3::Better2;
    }
    if p2.startup > p1.startup * fuzz {
        return Ordering3::Better1;
    }
    Ordering3::Equal
}

#[derive(PartialEq)]
enum Ordering3 {
    Better1,
    Better2,
    Equal,
    Different,
}

/// `bms_subset_compare` of two table sets: `Equal`, `Better1` when `a` is
/// the proper subset, `Better2` when `b` is, else `Different`.
fn subset_compare(a: u32, b: u32) -> Ordering3 {
    if a == b {
        Ordering3::Equal
    } else if a & !b == 0 {
        Ordering3::Better1
    } else if b & !a == 0 {
        Ordering3::Better2
    } else {
        Ordering3::Different
    }
}

/// `add_path`: `new` joins the paths unless one there is as cheap (1%
/// fuzz), as well ordered, needs no more outer tables and returns no more
/// rows; and it pushes out any it beats on all of those. A parameterized
/// path's order counts for nothing.
fn add_path(paths: &mut Vec<JoinPath>, new: JoinPath, startup_counts: bool) {
    let mut accept = true;
    let keys_of = |p: &JoinPath| (p.req_outer == 0 && p.keyed).then_some(p.class);
    // The list is kept in total cost order, a new path after every one
    // that costs as little (`insert_at`).
    paths.retain(|old| {
        if !accept {
            return true;
        }
        let costs = fuzzy_cmp(&new, old, 1.01, startup_counts);
        if costs == Ordering3::Different {
            return true;
        }
        // Pathkeys: a keyed path's are better than none, and two keyed on
        // different classes are `PATHKEYS_DIFFERENT`: both stay.
        let keys = match (keys_of(&new), keys_of(old)) {
            (Some(_), None) => Ordering3::Better1,
            (None, Some(_)) => Ordering3::Better2,
            (Some(x), Some(y)) if x != y => return true,
            _ => Ordering3::Equal,
        };
        let outer = subset_compare(new.req_outer, old.req_outer);
        let new_at_most = matches!(outer, Ordering3::Equal | Ordering3::Better1);
        let old_at_most = matches!(outer, Ordering3::Equal | Ordering3::Better2);
        let mut remove_old = false;
        match costs {
            Ordering3::Equal => match keys {
                Ordering3::Better1 => {
                    if new_at_most && new.rows <= old.rows {
                        remove_old = true;
                    }
                }
                Ordering3::Better2 => {
                    if old_at_most && new.rows >= old.rows {
                        accept = false;
                    }
                }
                _ => match outer {
                    Ordering3::Equal => {
                        if new.rows < old.rows {
                            remove_old = true;
                        } else if new.rows > old.rows {
                            accept = false;
                        } else if fuzzy_cmp(&new, old, 1.000_000_000_1, startup_counts)
                            == Ordering3::Better1
                        {
                            remove_old = true;
                        } else {
                            accept = false;
                        }
                    }
                    Ordering3::Better1 if new.rows <= old.rows => remove_old = true,
                    Ordering3::Better2 if new.rows >= old.rows => accept = false,
                    _ => {}
                },
            },
            Ordering3::Better1 => {
                if keys != Ordering3::Better2 && new_at_most && new.rows <= old.rows {
                    remove_old = true;
                }
            }
            Ordering3::Better2 => {
                if keys != Ordering3::Better1 && old_at_most && new.rows >= old.rows {
                    accept = false;
                }
            }
            Ordering3::Different => {}
        }
        !remove_old
    });
    if accept {
        let at = paths
            .iter()
            .rposition(|old| new.total >= old.total)
            .map_or(0, |i| i + 1);
        paths.insert(at, new);
    }
}

/// `JoinMethod` for `a JOIN b` (`left`: a LEFT join, `a` its outer side).
/// `limit` is a LIMIT's rows (with its OFFSET), for which the plan is the
/// cheapest for that fraction of the join's rows
/// (`get_cheapest_fractional_path`); `None` for the whole result.
pub fn cheapest_join(a: &JoinSide, b: &JoinSide, left: bool, limit: Option<f64>) -> JoinMethod {
    choose_join(a, b, left, limit, true)
}

/// `JoinMethod` for `a FULL JOIN b` on one equality: a merge or hash full
/// join, either side outer (a FULL join is never a nested loop).
/// `mergeable`: the ON holds nothing but the equality; a merge full join
/// cannot apply a join filter (`select_mergejoin_clauses`).
pub fn cheapest_full_join(
    a: &JoinSide,
    b: &JoinSide,
    limit: Option<f64>,
    mergeable: bool,
) -> JoinMethod {
    let mut rel = JoinRel::new(limit.is_some());
    rel.add_full_paths(0, a, b, mergeable);
    rel.cheapest(limit, full_join_rows(a, b))
        .expect("a join has paths")
        .method
}

/// `cheapest_join` for an inner join left with no join clause, every
/// equality of its ON and WHERE in an equivalence class with a constant:
/// a nested loop over every pair of the two sides' rows, either side
/// outer.
pub fn cheapest_cross_join(a: &JoinSide, b: &JoinSide, limit: Option<f64>) -> JoinMethod {
    choose_join(a, b, false, limit, false)
}

/// `cheapest_join` for a LEFT join whose outer key a constant fixes: the
/// inner side gets the constant too (`reconsider_outer_join_clauses`), and
/// the ON's equality is implied by the two sides' quals, so it selects
/// every pair; a hash bucket's size stays the default's.
pub fn cheapest_implied_join(
    a: &JoinSide,
    b: &JoinSide,
    left: bool,
    limit: Option<f64>,
) -> JoinMethod {
    let implied = |s: &JoinSide| JoinSide {
        ndistinct: 1.0,
        default_ndistinct: true,
        unique: false,
        ..*s
    };
    choose_join(&implied(a), &implied(b), left, limit, true)
}

fn choose_join(
    a: &JoinSide,
    b: &JoinSide,
    left: bool,
    limit: Option<f64>,
    clause: bool,
) -> JoinMethod {
    // A join rel weighs startup costs only under a LIMIT
    // (`consider_startup`: a positive `tuple_fraction`).
    let mut rel = JoinRel::new(limit.is_some());
    rel.add_paths(0, a, b, left, clause);
    let rows = if clause {
        join_rows(a, b, left)
    } else {
        clamp_row_est(a.rows * b.rows)
    };
    rel.cheapest(limit, rows).expect("a join has paths").method
}

/// The path a join relation's planning picked (`JoinRel::cheapest`): which
/// pair of inputs it joins (the `combo` it was added under), how, and its
/// cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JoinChoice {
    pub combo: usize,
    pub method: JoinMethod,
    pub startup: f64,
    pub total: f64,
    /// The equivalence class whose order it comes out in, if any
    /// (`add_paths_on`'s class, `None` for a path in no order of use).
    pub class: Option<usize>,
    /// The path of each input it reads (`DEFAULT_INPUT`: the input's
    /// cheapest, or its cheapest in key order where the method reads it
    /// presorted).
    pub a_path: usize,
    pub b_path: usize,
    /// Its rows, and what reading it again costs (`cost_rescan`).
    pub rows: f64,
    pub rescan: (f64, f64),
    /// The tables (a bit each) it needs from an outer side; 0 for none.
    pub req_outer: u32,
}

impl JoinPath {
    fn choice(&self) -> JoinChoice {
        JoinChoice {
            combo: self.combo,
            method: self.method,
            startup: self.startup,
            total: self.total,
            class: self.keyed.then_some(self.class),
            a_path: self.a_path,
            b_path: self.b_path,
            rows: self.rows,
            rescan: self.rescan,
            req_outer: self.req_outer,
        }
    }
}

/// `JoinRel::add_paths_on`'s class for a join whose key order nothing above
/// it can use: no join clause still to come and no ORDER BY reads it.
pub const USELESS_KEYS: usize = usize::MAX;

/// One join relation's paths (`RelOptInfo::pathlist`), as `add_path` keeps
/// them across every pair of inputs it can be built from.
#[derive(Debug, Clone)]
pub struct JoinRel {
    paths: Vec<JoinPath>,
    startup_counts: bool,
    /// Added to the total of each path added from now on: the join filters
    /// the join evaluates on each of its rows (`qp_qual_cost`), which are
    /// the same for every path of one pair of inputs.
    extra: f64,
    /// The join filters' cost a tuple, for the paths `add_paths_general`
    /// adds: a nested loop evaluates them on every pair it reads
    /// (`final_cost_nestloop`'s `restrict_qual_cost`), a hash or merge join
    /// on each tuple its clause passes.
    filter: f64,
    /// Each parameterization's rows, as the first pair of input paths to
    /// make it sized them (`ParamPathInfo::ppi_rows`).
    ppi_rows: Vec<(u32, f64)>,
}

impl JoinRel {
    /// `startup_counts`: the query has a LIMIT (`consider_startup`).
    pub fn new(startup_counts: bool) -> JoinRel {
        JoinRel {
            paths: Vec::new(),
            startup_counts,
            extra: 0.0,
            filter: 0.0,
            ppi_rows: Vec::new(),
        }
    }

    /// The join filters' cost the paths added next pay on top of their own
    /// (`final_cost_*join`'s `qp_qual_cost` over the join's rows).
    pub fn set_extra_cost(&mut self, extra: f64) {
        self.extra = extra;
    }

    /// The join filters the paths `add_paths_general` adds next evaluate:
    /// their cost a tuple (`cost_qual_eval`).
    pub fn set_filter_cost(&mut self, per_tuple: f64) {
        self.filter = per_tuple;
    }

    /// `add_paths_to_joinrel` for `a` joined to `b` (`left`: a LEFT join,
    /// `a` its preserved side), each side outer in turn
    /// (`populate_joinrel_with_paths`), under `combo`. `clause`: the join
    /// has an equality to hash or merge on.
    pub fn add_paths(
        &mut self,
        combo: usize,
        a: &JoinSide,
        b: &JoinSide,
        left: bool,
        clause: bool,
    ) {
        self.add_paths_on(combo, 0, a, b, left, clause);
    }

    /// `add_paths`, the join's clause in equivalence class `class`: its
    /// merge joins' output is in that class's order (`cheapest_keyed_on`).
    /// A relation built on clauses of several classes gives each its own.
    /// `USELESS_KEYS`: the order is of no use above this relation, and
    /// `truncate_useless_pathkeys` leaves its merge joins unordered.
    pub fn add_paths_on(
        &mut self,
        combo: usize,
        class: usize,
        a: &JoinSide,
        b: &JoinSide,
        left: bool,
        clause: bool,
    ) {
        let startup_counts = self.startup_counts;
        let extra = self.extra;
        let paths = &mut self.paths;
        let useful = class != USELESS_KEYS;
        let mut consider = |(startup, total): (f64, f64), keyed: bool, method: JoinMethod| {
            add_path(
                paths,
                JoinPath {
                    startup,
                    total: total + extra,
                    keyed: keyed && useful,
                    class,
                    method,
                    combo,
                    a_path: DEFAULT_INPUT,
                    b_path: DEFAULT_INPUT,
                    rows: 0.0,
                    req_outer: 0,
                    rescan: (startup, total + extra),
                },
                startup_counts,
            );
        };
        // `populate_joinrel_with_paths` builds `a` outer first, then `b`;
        // each `add_paths_to_joinrel` in turn tries merge joins over two
        // Sorts (`sort_inner_and_outer`), then nested loops and merge joins
        // over an outer already in order (`match_unsorted_outer`), then hash
        // joins. A LEFT join with `b` outer is a right join: a hash right
        // join returns the unmatched `a` rows last, in its hash table's
        // order; a merge right join in their key's place. A nested loop with
        // `a` outer emits its rows in both sides' scan order.
        for b_outer in [false, true] {
            let (outer, inner) = if b_outer { (b, a) } else { (a, b) };
            let right = b_outer && left;
            let merge = |oi: bool, ii: bool| {
                if right {
                    JoinMethod::MergeRightB {
                        a_by_index: ii,
                        b_by_index: oi,
                    }
                } else if b_outer {
                    JoinMethod::MergeOuterB {
                        a_by_index: ii,
                        b_by_index: oi,
                    }
                } else {
                    JoinMethod::MergeOuterA {
                        a_by_index: oi,
                        b_by_index: ii,
                    }
                }
            };
            // `sort_inner_and_outer` sorts each side's cheapest path, unless
            // it is in the key's order already.
            let (os, is) = (outer.scan_ordered, inner.scan_ordered);
            if let Some(cost) = merge_join(outer, inner, os, is).filter(|_| clause) {
                consider(cost, true, merge(os, is));
            }
            let nest = if b_outer {
                JoinMethod::NestOuterB
            } else {
                JoinMethod::Other
            };
            // Under a LIMIT the outer's cheapest scan to start is a path of
            // its own, which nested loops and hash joins read too
            // (`match_unsorted_outer`, `hash_inner_and_outer`).
            let startup_outer =
                (startup_counts && outer.startup_scan != outer.scan).then_some(JoinSide {
                    scan: outer.startup_scan,
                    ..*outer
                });
            let outers: Vec<&JoinSide> = std::iter::once(outer)
                .chain(startup_outer.as_ref())
                .collect();
            // A right join cannot be a nested loop.
            if !right {
                let qual = if clause {
                    clause_cost(outer, inner)
                } else {
                    0.0
                };
                for o in &outers {
                    consider(nest_loop_with(o, inner, false, qual), false, nest);
                    consider(nest_loop_with(o, inner, true, qual), false, nest);
                }
            }
            // Merge and hash joins need a join clause to join on.
            if !clause {
                continue;
            }
            for (oi, ii) in [(true, false), (true, true)] {
                if let Some(cost) = merge_join(outer, inner, oi, ii) {
                    consider(cost, true, merge(oi, ii));
                }
            }
            let hash = if right {
                JoinMethod::HashRightB
            } else if b_outer {
                JoinMethod::HashOuterB
            } else if b.unique {
                JoinMethod::Other
            } else {
                JoinMethod::HashOuterA
            };
            for o in &outers {
                consider(hash_join(o, inner), false, hash);
            }
        }
    }

    /// `add_paths_to_joinrel` for `a FULL JOIN b` on its one equality, each
    /// side outer in turn: merge joins over two Sorts
    /// (`sort_inner_and_outer`), merge joins over an outer already in order
    /// (`match_unsorted_outer`, whose nested loops a FULL join cannot
    /// use), then hash joins. A merge full join's output is in no order an
    /// upper join can use: its NULL-extended rows break the key's order.
    /// `mergeable` false (a join filter besides the equality) leaves only
    /// the hash joins (`select_mergejoin_clauses`).
    pub fn add_full_paths(&mut self, combo: usize, a: &JoinSide, b: &JoinSide, mergeable: bool) {
        let startup_counts = self.startup_counts;
        let extra = self.extra;
        let paths = &mut self.paths;
        let mut consider = |(startup, total): (f64, f64), method: JoinMethod| {
            add_path(
                paths,
                JoinPath {
                    startup,
                    total: total + extra,
                    keyed: false,
                    class: USELESS_KEYS,
                    method,
                    combo,
                    a_path: DEFAULT_INPUT,
                    b_path: DEFAULT_INPUT,
                    rows: 0.0,
                    req_outer: 0,
                    rescan: (startup, total + extra),
                },
                startup_counts,
            );
        };
        for b_outer in [false, true] {
            let (outer, inner) = if b_outer { (b, a) } else { (a, b) };
            let merge = |oi: bool, ii: bool| {
                if b_outer {
                    JoinMethod::MergeFullB {
                        a_by_index: ii,
                        b_by_index: oi,
                    }
                } else {
                    JoinMethod::MergeFullA {
                        a_by_index: oi,
                        b_by_index: ii,
                    }
                }
            };
            let (os, is) = (outer.scan_ordered, inner.scan_ordered);
            if mergeable {
                if let Some(cost) = merge_join(outer, inner, os, is) {
                    consider(cost, merge(os, is));
                }
            }
            let startup_outer =
                (startup_counts && outer.startup_scan != outer.scan).then_some(JoinSide {
                    scan: outer.startup_scan,
                    ..*outer
                });
            let outers: Vec<&JoinSide> = std::iter::once(outer)
                .chain(startup_outer.as_ref())
                .collect();
            for (oi, ii) in [(true, false), (true, true)] {
                if !mergeable {
                    break;
                }
                if let Some(cost) = merge_join(outer, inner, oi, ii) {
                    consider(cost, merge(oi, ii));
                }
            }
            let hash = if b_outer {
                JoinMethod::HashFullB
            } else {
                JoinMethod::HashFullA
            };
            for o in &outers {
                consider(hash_join(o, inner), hash);
            }
        }
    }

    /// The cheapest path in the join key's order (a merge join's), which a
    /// merge join above on the same key reads unsorted
    /// (`get_cheapest_path_for_pathkeys`).
    pub fn cheapest_keyed(&self) -> Option<JoinChoice> {
        self.cheapest_keyed_on(0)
    }

    /// `cheapest_keyed` among the paths in equivalence class `class`'s
    /// order (`add_paths_on`).
    pub fn cheapest_keyed_on(&self, class: usize) -> Option<JoinChoice> {
        self.paths
            .iter()
            .filter(|p| p.keyed && p.class == class)
            .min_by(|p, q| {
                p.total
                    .total_cmp(&q.total)
                    .then(p.startup.total_cmp(&q.startup))
            })
            .map(JoinPath::choice)
    }

    /// `set_cheapest`: the cheapest in total, a tie to the cheaper startup;
    /// under a LIMIT of `limit` of the join's `rows`, any other path cheaper
    /// for that fraction (`get_cheapest_fractional_path`).
    pub fn cheapest(&self, limit: Option<f64>, rows: f64) -> Option<JoinChoice> {
        let total_cmp = |p: &JoinPath, q: &JoinPath| {
            p.total
                .total_cmp(&q.total)
                .then(p.startup.total_cmp(&q.startup))
        };
        let mut best = *self.paths.iter().min_by(|p, q| total_cmp(p, q))?;
        if let Some(limit) = limit {
            let fraction = if limit >= 1.0 && rows > 0.0 {
                limit / rows
            } else {
                limit
            };
            if fraction > 0.0 && fraction < 1.0 {
                let price = |p: &JoinPath| p.startup + fraction * (p.total - p.startup);
                for p in &self.paths {
                    if price(p) < price(&best) {
                        best = *p;
                    }
                }
            }
        }
        Some(best.choice())
    }
}

/// One path of a relation's pathlist, as a join above reads it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InPath {
    pub startup: f64,
    pub total: f64,
    /// The equivalence class whose order it comes out in, if that order is
    /// still of use (`truncate_useless_pathkeys`).
    pub class: Option<usize>,
    /// Its rows: its relation's, or fewer for a parameterized scan.
    pub rows: f64,
    /// The tables (a bit each) a parameterized scan needs from the outer
    /// side of a nested loop; 0 for a path that needs none.
    pub req_outer: u32,
    /// What reading it again costs (`cost_rescan`): `(startup, total)`.
    pub rescan: (f64, f64),
    /// A parameterized index or bitmap scan whose index quals are its join
    /// clause (`has_indexed_join_quals`, when that is the join's only
    /// clause).
    pub indexed: bool,
    /// For a parameterized scan, the outer side's key it is probed with: a
    /// Memoize's cache key (`get_memoize_path`).
    pub memo_key: Option<MemoKey>,
}

impl InPath {
    /// An unparameterized scan of `rows` rows, read again at the same
    /// cost (`cost_rescan`'s default).
    pub fn scan(startup: f64, total: f64, rows: f64, class: Option<usize>) -> InPath {
        InPath {
            startup,
            total,
            class,
            rows,
            req_outer: 0,
            rescan: (startup, total),
            indexed: false,
            memo_key: None,
        }
    }
}

/// A Memoize's cache key, the outer side's join key, as
/// `estimate_num_groups` and `get_expr_width` see it: its distinct count
/// (`get_variable_numdistinct`) and whether that is a default, its table's
/// tuples and rows, and its width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemoKey {
    pub ndistinct: f64,
    pub default: bool,
    pub tuples: f64,
    pub rel_rows: f64,
    pub width: i64,
}

/// A join's input relation with its whole pathlist (`RelOptInfo::pathlist`,
/// in total cost order) and `set_cheapest`'s picks, as
/// `add_paths_to_joinrel` reads it.
#[derive(Debug, Clone, Copy)]
pub struct RelPaths<'a> {
    /// Its statistics: rows, the join key's distinct count and uniqueness,
    /// width; its path costs are `paths`'.
    pub side: JoinSide,
    pub paths: &'a [InPath],
    pub cheapest_total: usize,
    pub cheapest_startup: usize,
    /// Its tables, a bit each.
    pub set: u32,
    /// The width of its rows (`reltarget->width`), which a Material or a
    /// Memoize over it stores.
    pub width: i64,
    /// A base table whose join key is unique: the join is
    /// `inner_unique` when it is the inner side (`innerrel_is_unique`).
    pub unique: bool,
    /// The table (a bit) whose column is this side's key in the join's
    /// clause: a scan of the other side parameterized by it is what
    /// enforces the clause in its index.
    pub key_table: u32,
}

/// `compare_path_costs` on total cost (startup breaking a tie), or on
/// startup cost (total breaking it).
fn compare_path_costs(p1: &InPath, p2: &InPath, by_startup: bool) -> std::cmp::Ordering {
    let (x1, y1, x2, y2) = if by_startup {
        (p1.startup, p1.total, p2.startup, p2.total)
    } else {
        (p1.total, p1.startup, p2.total, p2.startup)
    };
    x1.total_cmp(&x2).then(y1.total_cmp(&y2))
}

impl RelPaths<'_> {
    /// The relation read through its path `i`; `presorted`: that path is
    /// in the join key's order, which a merge join reads without a Sort.
    fn at(&self, i: usize, presorted: bool) -> JoinSide {
        let p = self.paths[i];
        JoinSide {
            rows: p.rows,
            scan: (p.startup, p.total),
            startup_scan: (p.startup, p.total),
            ordered: presorted.then_some((p.startup, p.total)),
            scan_ordered: presorted,
            ..self.side
        }
    }

    /// `get_cheapest_path_for_pathkeys`: the cheapest of its unparameterized
    /// paths in `class`'s order, by total cost or by startup, the first of
    /// equals.
    fn cheapest_in(&self, class: usize, by_startup: bool) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, p) in self.paths.iter().enumerate() {
            if let Some(b) = best {
                if compare_path_costs(&self.paths[b], p, by_startup) != std::cmp::Ordering::Greater
                {
                    continue;
                }
            }
            if p.class == Some(class) && p.req_outer == 0 {
                best = Some(i);
            }
        }
        best
    }

    /// Whether path `i` is in `class`'s order.
    fn sorted_on(&self, i: usize, class: Option<usize>) -> bool {
        class.is_some() && self.paths[i].class == class && self.paths[i].req_outer == 0
    }

    /// `generate_mergejoin_paths`' presorted inners for an outer path in
    /// order: the inner's cheapest path in `class`'s order by total cost,
    /// if it beats the cheapest-total path (when that is in order itself),
    /// then its cheapest by startup cost, if that is another path and beats
    /// the cheapest-total one on startup.
    fn presorted_inners(&self, class: usize, total_in_order: bool) -> Vec<usize> {
        let ict = self.cheapest_total;
        let (mut total_inner, startup_inner) = if total_in_order {
            (Some(ict), Some(ict))
        } else {
            (None, None)
        };
        let mut out = Vec::new();
        let beats = |ip: usize, c: Option<usize>, by_startup: bool| {
            c.is_none_or(|c| {
                compare_path_costs(&self.paths[ip], &self.paths[c], by_startup)
                    == std::cmp::Ordering::Less
            })
        };
        if let Some(ip) = self.cheapest_in(class, false) {
            if beats(ip, total_inner, false) {
                out.push(ip);
                total_inner = Some(ip);
            }
        }
        if let Some(ip) = self.cheapest_in(class, true) {
            if beats(ip, startup_inner, true) && Some(ip) != total_inner {
                out.push(ip);
            }
        }
        out
    }

    /// `cheapest_parameterized_paths`: the cheapest-total path, then every
    /// parameterized path in pathlist order.
    fn parameterized(&self) -> Vec<usize> {
        std::iter::once(self.cheapest_total)
            .chain((0..self.paths.len()).filter(|&i| self.paths[i].req_outer != 0))
            .collect()
    }
}

/// `relation_byte_size`: `tuples` rows of `width` bytes, each with a heap
/// tuple header (23, aligned to 24).
fn relation_byte_size(tuples: f64, width: i64) -> f64 {
    tuples * (maxalign(width.max(0) as u64) + 24) as f64
}

/// `work_mem` (4 MB) in bytes.
const WORK_MEM_BYTES: f64 = 4096.0 * 1024.0;

/// `cost_material` over `inner` (`width` bytes a row), with what a rescan
/// of it costs (`cost_rescan`): two operators a row stored, one a row
/// read again, and a page a block when it spills past `work_mem`.
fn material(inner: &InPath, width: i64) -> InPath {
    let nbytes = relation_byte_size(inner.rows, width);
    let spill = if nbytes > WORK_MEM_BYTES {
        (nbytes / 8192.0).ceil()
    } else {
        0.0
    };
    InPath {
        startup: inner.startup,
        total: inner.total + 2.0 * CPU_OPERATOR_COST * inner.rows + spill,
        class: inner.class,
        rows: inner.rows,
        req_outer: inner.req_outer,
        rescan: (0.0, CPU_OPERATOR_COST * inner.rows + spill),
        indexed: false,
        memo_key: None,
    }
}

/// `create_memoize_path` over the parameterized scan `inner` (`width`
/// bytes a row), called once per outer row (`calls`), with
/// `cost_memoize_rescan`'s rescan: the cache entries `hash_mem` holds, the
/// key's distinct values among the calls (`estimate_num_groups`, or every
/// call a new one when that estimate is a default), and from them the
/// share of rescans the cache answers and of entries it evicts.
fn memoize(inner: &InPath, key: &MemoKey, calls: f64, width: i64) -> InPath {
    let tuples = inner.rows;
    // `ExecEstimateCacheEntryOverheadBytes`: a MemoizeEntry (24) and a
    // MemoizeKey (24), and a MemoizeTuple (16) a row.
    let est_entry_bytes =
        relation_byte_size(tuples, width) + 24.0 + 24.0 + 16.0 * tuples + key.width as f64;
    let est_cache_entries = (HASH_MEM_BYTES as f64 / est_entry_bytes).floor();
    let ndistinct = if key.default {
        calls
    } else {
        let mut reldistinct = key.ndistinct.min(key.tuples);
        if reldistinct > 0.0 && key.rel_rows < key.tuples {
            reldistinct *=
                1.0 - ((key.tuples - key.rel_rows) / key.tuples).powf(key.tuples / reldistinct);
        }
        let n = clamp_row_est(reldistinct).ceil();
        n.min(calls).max(1.0)
    };
    let held = est_cache_entries.min(ndistinct);
    let evict_ratio = 1.0 - held / ndistinct;
    let hit_ratio = (1.0 / ndistinct * held - ndistinct / calls).max(0.0);
    let mut total = inner.total * (1.0 - hit_ratio) + CPU_OPERATOR_COST;
    total += CPU_TUPLE_COST * evict_ratio;
    total += CPU_OPERATOR_COST / 10.0 * evict_ratio * tuples;
    total += CPU_TUPLE_COST + CPU_OPERATOR_COST * tuples;
    let startup = inner.startup * (1.0 - hit_ratio) + CPU_TUPLE_COST;
    InPath {
        startup: inner.startup + CPU_TUPLE_COST,
        total: inner.total + CPU_TUPLE_COST,
        class: None,
        rows: inner.rows,
        req_outer: inner.req_outer,
        rescan: (startup, total),
        indexed: false,
        memo_key: None,
    }
}

/// `initial_cost_nestloop` and `final_cost_nestloop` for `outer` rows each
/// rescanning `inner`: every rescan's startup, and its run -- all of it,
/// or, when the inner side is unique (`unique`: the semi-join factors'
/// match fraction and match count), the share a scan reads before its
/// first match, an unmatched outer row costing an indexed inner scan its
/// first tuple's share and any other inner scan all of it -- and
/// `cpu_tuple_cost` plus the join quals still to check (`qual`) on each
/// pair it reads.
fn nestloop_cost(
    outer: &InPath,
    inner: &InPath,
    unique: Option<(f64, f64)>,
    indexed: bool,
    qual: f64,
) -> (f64, f64) {
    let (rescan_start, rescan_total) = inner.rescan;
    let startup = outer.startup + inner.startup;
    let mut run = outer.total - outer.startup;
    if outer.rows > 1.0 {
        run += (outer.rows - 1.0) * rescan_start;
    }
    let inner_run = inner.total - inner.startup;
    let inner_rescan_run = rescan_total - rescan_start;
    let outer_rows = outer.rows.max(1.0);
    let inner_rows = inner.rows.max(1.0);
    let ntuples = match unique {
        None => {
            run += inner_run;
            if outer.rows > 1.0 {
                run += (outer.rows - 1.0) * inner_rescan_run;
            }
            outer_rows * inner_rows
        }
        Some((match_frac, match_count)) => {
            let mut matched = (outer_rows * match_frac).round_ties_even();
            let mut unmatched = outer_rows - matched;
            let scan_frac = 2.0 / (match_count + 1.0);
            let mut ntuples = matched * inner_rows * scan_frac;
            if indexed {
                run += inner_run * scan_frac;
                if matched > 1.0 {
                    run += (matched - 1.0) * inner_rescan_run * scan_frac;
                }
                run += unmatched * inner_rescan_run / inner_rows;
            } else {
                ntuples += unmatched * inner_rows;
                run += inner_run;
                if unmatched >= 1.0 {
                    unmatched -= 1.0;
                } else {
                    matched -= 1.0;
                }
                if matched > 0.0 {
                    run += matched * inner_rescan_run * scan_frac;
                }
                if unmatched > 0.0 {
                    run += unmatched * inner_rescan_run;
                }
            }
            ntuples
        }
    };
    run += (CPU_TUPLE_COST + qual) * ntuples;
    (startup, startup + run)
}

/// What a hash join's own rescan costs (`cost_rescan`): with one batch,
/// the hash table is kept, so only its run again.
fn hash_rescan((startup, total): (f64, f64), inner: &JoinSide) -> (f64, f64) {
    let nbatch = inner.width.map_or(1, |w| hash_table_size(inner.rows, w).1);
    if nbatch == 1 {
        (0.0, total - startup)
    } else {
        (startup, total)
    }
}

impl JoinRel {
    /// `add_paths_to_joinrel` for `a` joined to `b` over their whole
    /// pathlists (`left`: a LEFT join, `a` preserved), each side outer in
    /// turn (`populate_joinrel_with_paths`), under `combo`, for a relation
    /// of `rows` rows:
    ///
    /// - `sort_inner_and_outer`: a merge join of the two cheapest-total
    ///   paths, each sorted unless it is in its key's order already;
    /// - `match_unsorted_outer`: for every outer path in pathlist order, a
    ///   nested loop over each of the inner's `cheapest_parameterized_paths`
    ///   that the outer side's tables can parameterize -- over a Memoize of
    ///   a parameterized one too -- and over a Material of its
    ///   cheapest-total path (none for a right join), in the outer path's
    ///   order; and, when the outer path is in its key's order, merge joins
    ///   with the inner's cheapest-total path sorted and with its cheapest
    ///   paths already in order (`generate_mergejoin_paths`);
    /// - `hash_inner_and_outer`: a hash join of the outer's cheapest-startup
    ///   path over the inner's cheapest-total one, then of every pair of
    ///   the two sides' `cheapest_parameterized_paths`.
    ///
    /// A path that still needs outer tables is built only where
    /// `param_source` (`param_source_rels`: the tables an outer join keeps
    /// from joining this relation directly) holds one of them, its rows the
    /// parameterization's (`get_joinrel_parampathinfo`). `a_class` /
    /// `b_class`: the equivalence class of each side's key, whose order a
    /// merge join reads presorted and leaves (the outer's; none for a right
    /// join, `build_join_pathkeys`). `useful`: whether an order is still of
    /// use above the new relation (`truncate_useless_pathkeys`). A join on
    /// several clauses sorts on all of them: no single class's order serves
    /// it.
    #[allow(clippy::too_many_arguments)]
    pub fn add_paths_general(
        &mut self,
        combo: usize,
        a_class: Option<usize>,
        b_class: Option<usize>,
        useful: &dyn Fn(usize) -> bool,
        a: &RelPaths<'_>,
        b: &RelPaths<'_>,
        left: bool,
        rows: f64,
        param_source: u32,
    ) {
        let startup_counts = self.startup_counts;
        let extra = self.extra;
        let qp = self.filter;
        let ppi = &mut self.ppi_rows;
        let paths = &mut self.paths;
        let multi = a.side.extra.n > 0;
        let (a_class, b_class) = if multi {
            (None, None)
        } else {
            (a_class, b_class)
        };
        // `get_joinrel_parampathinfo`: a parameterized path's rows, the
        // first pair of input paths to make that parameterization sizing it
        // (`get_parameterized_joinrel_size`: their rows through the join's
        // clauses), at most the relation's.
        let mut param_rows = |req: u32, (a_path, b_path): (usize, usize)| -> f64 {
            if req == 0 {
                return rows;
            }
            if let Some((_, r)) = ppi.iter().find(|(o, _)| *o == req) {
                return *r;
            }
            let (pa, pb) = (a.at(a_path, false), b.at(b_path, false));
            let r = join_rows(&pa, &pb, left).min(rows);
            ppi.push((req, r));
            r
        };
        let mut consider = |(startup, total): (f64, f64),
                            order: Option<usize>,
                            method: JoinMethod,
                            ids: (usize, usize),
                            rescan: Option<(f64, f64)>,
                            req_outer: u32| {
            // A parameterized path's order counts for nothing.
            let order = order.filter(|c| useful(*c) && req_outer == 0);
            let path_rows = param_rows(req_outer, ids);
            add_path(
                paths,
                JoinPath {
                    startup,
                    total: total + extra,
                    keyed: order.is_some(),
                    class: order.unwrap_or(USELESS_KEYS),
                    method,
                    combo,
                    a_path: ids.0,
                    b_path: ids.1,
                    rows: path_rows,
                    req_outer,
                    rescan: rescan.unwrap_or((startup, total + extra)),
                },
                startup_counts,
            );
        };
        // A path still needing outer tables is made only where some join
        // above cannot join those tables directly (`param_source_rels`).
        let sensible = |req: u32| req == 0 || req & param_source != 0;
        for b_outer in [false, true] {
            let (outer, inner, o_class, i_class) = if b_outer {
                (b, a, b_class, a_class)
            } else {
                (a, b, a_class, b_class)
            };
            let right = b_outer && left;
            let ids = |o: usize, i: usize| if b_outer { (i, o) } else { (o, i) };
            let merge = |oi: bool, ii: bool| {
                if right {
                    JoinMethod::MergeRightB {
                        a_by_index: ii,
                        b_by_index: oi,
                    }
                } else if b_outer {
                    JoinMethod::MergeOuterB {
                        a_by_index: ii,
                        b_by_index: oi,
                    }
                } else {
                    JoinMethod::MergeOuterA {
                        a_by_index: oi,
                        b_by_index: ii,
                    }
                }
            };
            let merge_order = if right { None } else { o_class };
            let (oct, ict) = (outer.cheapest_total, inner.cheapest_total);
            // The inner side unique (`inner_unique`): its semi-join factors,
            // the join's selectivity the share of outer rows that match and
            // the inner relation's rows the matches each finds.
            let unique = (inner.unique && !right).then(|| {
                let sel = join_selectivity(&outer.side, &inner.side);
                (sel, (sel * inner.side.rows / sel).max(1.0))
            });
            // A presorted inner a merge join marks and restores: a base
            // table's index scan can; a join's output cannot, and is
            // materialised.
            let markable = inner.set.count_ones() == 1;
            // sort_inner_and_outer: the two cheapest-total paths.
            let (os, is) = (outer.sorted_on(oct, o_class), inner.sorted_on(ict, i_class));
            if let Some(cost) =
                merge_join_full(&outer.at(oct, os), &inner.at(ict, is), os, is, qp, markable)
            {
                consider(cost, merge_order, merge(os, is), ids(oct, ict), None, 0);
            }
            // match_unsorted_outer.
            let nest = if b_outer {
                JoinMethod::NestOuterB
            } else {
                JoinMethod::Other
            };
            let qual = clause_cost(&outer.side, &inner.side) + qp;
            let mat = material(&inner.paths[ict], inner.width);
            let inner_params = inner.parameterized();
            for (oi, op) in outer.paths.iter().enumerate() {
                if op.req_outer & inner.set != 0 {
                    continue;
                }
                // `calc_nestloop_required_outer`: what the two paths need
                // that the outer side does not hold; such a path is made
                // where it is sensible, or where the inner needs the outer
                // side and another table too (`allow_star_schema_join`).
                let nest_req = |ip: &InPath| {
                    let req = (op.req_outer | ip.req_outer) & !outer.set;
                    let star = ip.req_outer & outer.set != 0 && ip.req_outer & !outer.set != 0;
                    (sensible(req) || star).then_some(req)
                };
                if !right {
                    for &ii in &inner_params {
                        let ip = &inner.paths[ii];
                        // An inner parameterized by the clause's outer table
                        // enforces the clause in its index: none left to
                        // check on each pair. One parameterized by another
                        // table of the class leaves the clause a join qual
                        // (`create_nestloop_path` moves only the clauses it
                        // can evaluate into it).
                        let (q, indexed) = if ip.req_outer != 0 && ip.req_outer == outer.key_table {
                            (qp, ip.indexed && qp == 0.0)
                        } else {
                            (qual, false)
                        };
                        if let Some(req) = nest_req(ip) {
                            consider(
                                nestloop_cost(op, ip, unique, indexed, q),
                                op.class,
                                nest,
                                ids(oi, ii),
                                None,
                                req,
                            );
                        }
                        let memo = ip.memo_key.filter(|_| {
                            ip.req_outer != 0
                                && outer.side.rows >= 2.0
                                && !(unique.is_some() && (qp > 0.0 || multi))
                        });
                        if let Some(key) = memo {
                            let m = memoize(ip, &key, op.rows, inner.width);
                            if let Some(req) = nest_req(&m) {
                                consider(
                                    nestloop_cost(op, &m, unique, false, q),
                                    op.class,
                                    nest,
                                    ids(oi, ii),
                                    None,
                                    req,
                                );
                            }
                        }
                    }
                    if let Some(req) = nest_req(&mat) {
                        consider(
                            nestloop_cost(op, &mat, unique, false, qual),
                            op.class,
                            nest,
                            ids(oi, ict),
                            None,
                            req,
                        );
                    }
                }
                // `generate_mergejoin_paths`; `calc_non_nestloop_required_outer`.
                let req = op.req_outer | inner.paths[ict].req_outer;
                if !sensible(req) || !outer.sorted_on(oi, o_class) {
                    continue;
                }
                let o = outer.at(oi, true);
                if let Some(cost) = merge_join_full(&o, &inner.at(ict, is), true, is, qp, markable)
                {
                    consider(cost, merge_order, merge(true, is), ids(oi, ict), None, req);
                }
                let Some(ic) = i_class else { continue };
                for ip in inner.presorted_inners(ic, is) {
                    if let Some(cost) =
                        merge_join_full(&o, &inner.at(ip, true), true, true, qp, markable)
                    {
                        consider(cost, merge_order, merge(true, true), ids(oi, ip), None, req);
                    }
                }
            }
            // hash_inner_and_outer: the outer's cheapest-startup path over
            // the inner's cheapest-total, then every pair of the two sides'
            // `cheapest_parameterized_paths`.
            let hash = if right {
                JoinMethod::HashRightB
            } else if b_outer {
                JoinMethod::HashOuterB
            } else if b.side.unique {
                JoinMethod::Other
            } else {
                JoinMethod::HashOuterA
            };
            let cso = outer.cheapest_startup;
            let mut pairs = vec![(cso, ict)];
            for &o in &outer.parameterized() {
                if outer.paths[o].req_outer & inner.set != 0 {
                    continue;
                }
                for &i in &inner.parameterized() {
                    if inner.paths[i].req_outer & outer.set != 0 || (o, i) == (cso, ict) {
                        continue;
                    }
                    pairs.push((o, i));
                }
            }
            for (o, i) in pairs {
                let req = outer.paths[o].req_outer | inner.paths[i].req_outer;
                if !sensible(req) {
                    continue;
                }
                let isd = inner.at(i, false);
                let cost = hash_join_qp(&outer.at(o, false), &isd, qp);
                let rescan = hash_rescan((cost.0, cost.1 + extra), &isd);
                consider(cost, None, hash, ids(o, i), Some(rescan), req);
            }
        }
    }

    /// [`JoinRel::add_paths_general`] for `a FULL JOIN b` on its one
    /// equality: merge joins (`sort_inner_and_outer`, then each outer path
    /// already in its key's order with the inner sorted or presorted) and
    /// hash joins, never a nested loop; the output is in no order.
    #[allow(clippy::too_many_arguments)]
    pub fn add_full_paths_general(
        &mut self,
        combo: usize,
        a_class: Option<usize>,
        b_class: Option<usize>,
        a: &RelPaths<'_>,
        b: &RelPaths<'_>,
        rows: f64,
    ) {
        let startup_counts = self.startup_counts;
        let extra = self.extra;
        let paths = &mut self.paths;
        let mut consider = |(startup, total): (f64, f64),
                            method: JoinMethod,
                            (a_path, b_path): (usize, usize),
                            rescan: Option<(f64, f64)>| {
            add_path(
                paths,
                JoinPath {
                    startup,
                    total: total + extra,
                    keyed: false,
                    class: USELESS_KEYS,
                    method,
                    combo,
                    a_path,
                    b_path,
                    rows,
                    req_outer: 0,
                    rescan: rescan.unwrap_or((startup, total + extra)),
                },
                startup_counts,
            );
        };
        for b_outer in [false, true] {
            let (outer, inner, o_class, i_class) = if b_outer {
                (b, a, b_class, a_class)
            } else {
                (a, b, a_class, b_class)
            };
            let ids = |o: usize, i: usize| if b_outer { (i, o) } else { (o, i) };
            let merge = |oi: bool, ii: bool| {
                if b_outer {
                    JoinMethod::MergeFullB {
                        a_by_index: ii,
                        b_by_index: oi,
                    }
                } else {
                    JoinMethod::MergeFullA {
                        a_by_index: oi,
                        b_by_index: ii,
                    }
                }
            };
            let (oct, ict) = (outer.cheapest_total, inner.cheapest_total);
            let (os, is) = (outer.sorted_on(oct, o_class), inner.sorted_on(ict, i_class));
            if let Some(cost) = merge_join(&outer.at(oct, os), &inner.at(ict, is), os, is) {
                consider(cost, merge(os, is), ids(oct, ict), None);
            }
            for oi in 0..outer.paths.len() {
                if outer.paths[oi].req_outer != 0 || !outer.sorted_on(oi, o_class) {
                    continue;
                }
                let o = outer.at(oi, true);
                if let Some(cost) = merge_join(&o, &inner.at(ict, is), true, is) {
                    consider(cost, merge(true, is), ids(oi, ict), None);
                }
                let Some(ic) = i_class else { continue };
                for ip in inner.presorted_inners(ic, is) {
                    if let Some(cost) = merge_join(&o, &inner.at(ip, true), true, true) {
                        consider(cost, merge(true, true), ids(oi, ip), None);
                    }
                }
            }
            let hash = if b_outer {
                JoinMethod::HashFullB
            } else {
                JoinMethod::HashFullA
            };
            let cso = outer.cheapest_startup;
            let i = inner.at(ict, false);
            for o in std::iter::once(cso).chain((oct != cso).then_some(oct)) {
                let cost = hash_join(&outer.at(o, false), &i);
                let rescan = hash_rescan((cost.0, cost.1 + extra), &i);
                consider(cost, hash, ids(o, ict), Some(rescan));
            }
        }
    }

    /// The relation's paths in `add_path`'s order (by total cost), each as
    /// the plan it is.
    pub fn pathlist(&self) -> Vec<JoinChoice> {
        self.paths.iter().map(JoinPath::choice).collect()
    }

    /// `set_cheapest`: the indexes in `pathlist` of the cheapest-total
    /// unparameterized path and the cheapest-startup one; of two that cost
    /// the same, the earlier, unless the later is in an order and the
    /// earlier in none.
    pub fn set_cheapest(&self) -> Option<(usize, usize)> {
        let cost = |j: usize| InPath::scan(self.paths[j].startup, self.paths[j].total, 0.0, None);
        let mut best: Option<(usize, usize)> = None;
        for (i, p) in self.paths.iter().enumerate() {
            if p.req_outer != 0 {
                continue;
            }
            let Some((mut total, mut startup)) = best else {
                best = Some((i, i));
                continue;
            };
            let better_keys = |j: usize| !self.paths[j].keyed && p.keyed;
            let cmp = compare_path_costs(&cost(startup), &cost(i), true);
            if cmp == std::cmp::Ordering::Greater
                || (cmp == std::cmp::Ordering::Equal && better_keys(startup))
            {
                startup = i;
            }
            let cmp = compare_path_costs(&cost(total), &cost(i), false);
            if cmp == std::cmp::Ordering::Greater
                || (cmp == std::cmp::Ordering::Equal && better_keys(total))
            {
                total = i;
            }
            best = Some((total, startup));
        }
        best
    }
}

/// A base table's pathlist from its candidate scans, in the order its
/// scans are built: each put in by `add_path` (`startup_counts`: under a
/// LIMIT). The candidates' indexes that survive, in pathlist order, and
/// the positions in that list of the cheapest-total and cheapest-startup
/// unparameterized ones (`set_cheapest`).
pub fn base_pathlist(cands: &[InPath], startup_counts: bool) -> Option<(Vec<usize>, usize, usize)> {
    let mut rel = JoinRel::new(startup_counts);
    for (i, c) in cands.iter().enumerate() {
        add_path(
            &mut rel.paths,
            JoinPath {
                startup: c.startup,
                total: c.total,
                keyed: c.class.is_some(),
                class: c.class.unwrap_or(USELESS_KEYS),
                method: JoinMethod::Other,
                combo: i,
                a_path: DEFAULT_INPUT,
                b_path: DEFAULT_INPUT,
                rows: c.rows,
                req_outer: c.req_outer,
                rescan: c.rescan,
            },
            startup_counts,
        );
    }
    let (t, s) = rel.set_cheapest()?;
    Some((rel.paths.iter().map(|p| p.combo).collect(), t, s))
}

/// The path PostgreSQL runs for an ORDER BY of one key and a LIMIT over a
/// join relation of `rows` rows `width` bytes wide whose pathlist is
/// `paths`: `create_ordered_paths` puts in the ordered relation each path
/// already in the key's class order (`class`) as it is, and the
/// cheapest-total one (`cheapest_total`) under a Sort the LIMIT bounds
/// (`cost_sort` with `limit_tuples`), by `add_path`; then
/// [`limit_choice`] over those. The index in `paths` of the path it runs,
/// and whether it is in the key's order already (no Sort).
#[allow(clippy::too_many_arguments)]
pub fn ordered_limit_choice(
    paths: &[JoinChoice],
    cheapest_total: usize,
    rows: f64,
    width: i64,
    class: usize,
    count: f64,
    offset: f64,
) -> Option<(usize, bool)> {
    let mut ordered = JoinRel::new(true);
    for (i, p) in paths.iter().enumerate() {
        let (startup, total) = if p.class == Some(class) {
            (p.startup, p.total)
        } else if i == cheapest_total {
            cost_sort(p.total, rows, width, Some(count + offset))
        } else {
            continue;
        };
        add_path(
            &mut ordered.paths,
            JoinPath {
                startup,
                total,
                keyed: true,
                class,
                method: p.method,
                combo: i,
                a_path: DEFAULT_INPUT,
                b_path: DEFAULT_INPUT,
                rows,
                req_outer: 0,
                rescan: (startup, total),
            },
            true,
        );
    }
    let list = ordered.pathlist();
    let pick = list.get(limit_choice(&list, rows, count, offset)?)?.combo;
    Some((pick, paths[pick].class == Some(class)))
}

/// The path PostgreSQL runs under a LIMIT of `count` rows after `offset`
/// over a relation estimated at `rows`, whose pathlist is `paths`: each
/// under a Limit (`create_limit_path`, whose `adjust_limit_rows_costs`
/// charges the share of its run the rows it reads take), put in the final
/// relation by `add_path` (startup costs counting, `tuple_fraction` being
/// positive), and that relation's cheapest-total path
/// (`get_cheapest_fractional_path` with no cursor fraction). The index in
/// `paths` of the one it picks.
pub fn limit_choice(paths: &[JoinChoice], rows: f64, count: f64, offset: f64) -> Option<usize> {
    let mut finals = JoinRel::new(true);
    for (i, p) in paths.iter().enumerate() {
        let (mut startup, mut total) = (p.startup, p.total);
        let mut left = rows;
        if offset > 0.0 {
            let offset_rows = offset.min(rows);
            if rows > 0.0 {
                startup += (p.total - p.startup) * offset_rows / rows;
            }
            left = (left - offset_rows).max(1.0);
        }
        let count_rows = count.min(left);
        if rows > 0.0 {
            total = startup + (p.total - p.startup) * count_rows / rows;
        }
        add_path(
            &mut finals.paths,
            JoinPath {
                startup,
                total,
                keyed: p.class.is_some(),
                class: p.class.unwrap_or(USELESS_KEYS),
                method: p.method,
                combo: i,
                a_path: DEFAULT_INPUT,
                b_path: DEFAULT_INPUT,
                rows: count_rows.max(1.0),
                req_outer: 0,
                rescan: (startup, total),
            },
            true,
        );
    }
    let (t, _) = finals.set_cheapest()?;
    Some(finals.paths[t].combo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.006
    }

    /// PostgreSQL 15.19, fresh tables of 20 rows: jp (a int primary key, b
    /// int) and jq (a int primary key, c int), 10 pages of 226 rows each;
    /// jn and jm the same without a key.
    fn pk() -> JoinSide {
        JoinSide {
            rows: 2260.0,
            tuples: 2260.0,
            scan: (0.0, 32.60),
            startup_scan: (0.0, 32.60),
            ordered: Some((0.155, 82.055)),
            scan_ordered: false,
            unique: true,
            ndistinct: 2260.0,
            default_ndistinct: false,
            key_ops: 0.0,
            rel_rows: 2260.0,
            width: None,
            extra: ExtraKeys::default(),
        }
    }
    fn plain() -> JoinSide {
        JoinSide {
            ordered: None,
            unique: false,
            ndistinct: 200.0,
            default_ndistinct: true,
            ..pk()
        }
    }

    #[test]
    fn a_hash_table_past_work_mem_is_split_into_batches() {
        // PostgreSQL 15.19, five 20-row tables never analysed (2,040 rows
        // estimated each), t1.a a primary key: the top Hash Join of
        // `t0 join t1 on t0.b = t1.c join t2 on t2.a = t1.a join t3 on
        // t2.a = t3.a join t4 on t3.b = t4.c` hashes t0-t1-t3-t4 (212,242
        // rows, width 16, 1152.59..3909.45) under a Seq Scan of t2 (width
        // 4): Hash Join (7599.47..11075.59), two batches.
        assert_eq!(hash_table_size(212_242.0, 16), (262_144, 2));
        assert_eq!(hash_table_size(20_808.0, 8).1, 1);
        let t2 = JoinSide {
            rows: 2040.0,
            tuples: 2040.0,
            rel_rows: 2040.0,
            scan: (0.0, 30.40),
            startup_scan: (0.0, 30.40),
            width: Some(4),
            ..plain()
        };
        let hashed = JoinSide {
            rows: 212_242.0,
            tuples: 2040.0,
            rel_rows: 2040.0,
            scan: (1152.59, 3909.45),
            startup_scan: (1152.59, 3909.45),
            ndistinct: 2040.0,
            default_ndistinct: false,
            width: Some(16),
            ..plain()
        };
        let (s, t) = hash_join(&t2, &hashed);
        assert!(close(s, 7599.47) && close(t, 11075.59), "{s} {t}");
        // Rotated right by log2(nbuckets), masked by nbatch - 1.
        assert_eq!(hash_batch(1 << 18, 262_144, 2), 1);
        assert_eq!(hash_batch((1 << 18) - 1, 262_144, 2), 0);
        assert_eq!(hash_batch(u32::MAX, 1024, 1), 0);
    }

    #[test]
    fn joins_cost_as_postgres_does() {
        // jp join jq on a: Hash Join (60.85..99.39).
        let (s, t) = hash_join(&pk(), &pk());
        assert!(close(s, 60.85) && close(t, 99.39), "{s} {t}");
        // Merge Join over both index scans (0.31..198.01).
        let (s, t) = merge_join(&pk(), &pk(), true, true).unwrap();
        assert!(close(s, 0.31) && close(t, 198.01), "{s} {t}");
        // jn join jm on a: Merge Join over two Sorts (317.01..711.38).
        let (s, t) = merge_join(&plain(), &plain(), false, false).unwrap();
        assert!(close(s, 317.01) && close(t, 711.38), "{s} {t}");
        assert_eq!(join_rows(&plain(), &plain(), false), 25538.0);
        // jp join jq on jp.b = jq.c + 3: the clause evaluates `+` too, so
        // the Merge Join over two Sorts costs 317.01..780.88.
        let expr = JoinSide {
            key_ops: 1.0,
            ..plain()
        };
        let (s, t) = merge_join(&plain(), &expr, false, false).unwrap();
        assert!(close(s, 317.01) && close(t, 780.88), "{s} {t}");
    }

    #[test]
    fn the_method_postgres_picks_for_a_whole_join() {
        // Two keys: a Hash Join, jp probing (60.85..99.39), one match a row.
        assert_eq!(cheapest_join(&pk(), &pk(), false, None), JoinMethod::Other);
        // Two plain columns, many to many: a Merge Join over two Sorts.
        assert_eq!(
            cheapest_join(&plain(), &plain(), false, None),
            JoinMethod::MergeOuterA {
                a_by_index: false,
                b_by_index: false
            }
        );
    }

    #[test]
    fn nested_loops_cost_as_postgres_does() {
        // jp join jq on b = c, under a LIMIT: a Nested Loop over a
        // Materialize of jq (0.00..76684.85).
        let (s, t) = nest_loop(&plain(), &plain(), true);
        assert!(close(s, 0.0) && close(t, 76684.85), "{s} {t}");
    }

    #[test]
    fn the_method_postgres_picks_under_a_where_or_a_limit() {
        // `where jp.a >= 2 and jq.c < 4`: a bitmap heap scan of jp
        // (9.99..29.40) and a seq scan of jq (0.00..38.25), 753 rows each.
        // The two hash joins cost the same in total; jq outer starts sooner
        // (38.82..177.89), so `add_path` keeps it.
        let jp = JoinSide {
            rows: 753.0,
            rel_rows: 753.0,
            scan: (9.99, 29.40),
            startup_scan: (0.0, 38.25),
            ..plain()
        };
        let jq = JoinSide {
            rows: 753.0,
            rel_rows: 753.0,
            scan: (0.0, 38.25),
            startup_scan: (0.0, 38.25),
            ..plain()
        };
        // The scans' costs are as EXPLAIN rounds them, so to a cent.
        let (s, t) = hash_join(&jq, &jp);
        assert!(
            (s - 38.82).abs() < 0.01 && (t - 177.89).abs() < 0.01,
            "{s} {t}"
        );
        assert_eq!(cheapest_join(&jp, &jq, false, None), JoinMethod::HashOuterB);
        // `limit 5` over the whole join: the nested loop, jp outer.
        assert_eq!(
            cheapest_join(&plain(), &plain(), false, Some(5.0)),
            JoinMethod::Other
        );
        // `limit 3000`: the hash join again (60.85..992.93).
        assert_eq!(
            cheapest_join(&plain(), &plain(), false, Some(3000.0)),
            JoinMethod::HashOuterA
        );
    }

    #[test]
    fn the_plans_postgres_picks_for_an_equivalence_class_with_a_constant() {
        // `where jp.b = 2`: both sides filtered to 11 rows (38.25).
        let side = JoinSide {
            rows: 11.0,
            rel_rows: 11.0,
            scan: (0.0, 38.25),
            startup_scan: (0.0, 38.25),
            ..plain()
        };
        // An inner join has no join clause left: a Nested Loop over a
        // Materialize (0.00..78.04).
        let (s, t) = nest_loop_with(&side, &side, true, 0.0);
        assert!(close(s, 0.0) && close(t, 78.04), "{s} {t}");
        assert_eq!(cheapest_cross_join(&side, &side, None), JoinMethod::Other);
        // A LEFT join keeps the ON, selecting every pair: the Hash Left
        // Join (38.39..77.89) is fuzzily as cheap as the Nested Loop Left
        // Join (0.00..78.34), which starts sooner.
        let (s, t) = nest_loop(&side, &side, true);
        assert!(close(s, 0.0) && close(t, 78.34), "{s} {t}");
        assert_eq!(
            cheapest_implied_join(&side, &side, true, None),
            JoinMethod::Other
        );
    }

    #[test]
    fn a_join_of_a_join_costs_as_postgres_does() {
        // jp join jq on b = c join jn on jq.a = jn.a: jq and jn first, a
        // Hash Join (60.85..99.39, 2,260 rows), then a Merge Join of its
        // output sorted by jq.c with jp sorted by b (383.81..778.18).
        let mut rel = JoinRel::new(false);
        rel.add_paths(0, &plain(), &pk(), false, true);
        let first = rel.cheapest(None, 2260.0).unwrap();
        assert!(
            close(first.startup, 60.85) && close(first.total, 99.39),
            "{first:?}"
        );
        let joined = JoinSide {
            scan: (first.startup, first.total),
            startup_scan: (first.startup, first.total),
            ordered: None,
            ..plain()
        };
        let (s, t) = merge_join(&joined, &plain(), false, false).unwrap();
        assert!(close(s, 383.81) && close(t, 778.18), "{s} {t}");
    }

    #[test]
    fn a_merge_join_reads_a_pair_merged_on_its_key_unsorted() {
        // jp join jq on b = c join jn on jp.b = jn.d: one equivalence class.
        // jp-jq merge (317.01..711.38, 25,538 rows, in key order), then a
        // Merge Join with jn sorted outer and that pair, materialised,
        // inner (475.52..5209.87).
        let mut rel = JoinRel::new(false);
        rel.add_paths(0, &plain(), &plain(), false, true);
        let pair = rel.cheapest(None, 25538.0).unwrap();
        assert_eq!(Some(pair), rel.cheapest_keyed());
        let joined = JoinSide {
            rows: 25538.0,
            scan: (pair.startup, pair.total),
            startup_scan: (pair.startup, pair.total),
            ordered: Some((pair.startup, pair.total)),
            scan_ordered: true,
            ..plain()
        };
        let mut top = JoinRel::new(false);
        top.add_paths(0, &joined, &plain(), false, true);
        let best = top.cheapest(None, 288579.0).unwrap();
        assert!(
            close(best.startup, 475.52) && close(best.total, 5209.87),
            "{best:?}"
        );
        assert_eq!(
            best.method,
            JoinMethod::MergeOuterB {
                a_by_index: true,
                b_by_index: false
            }
        );
    }

    #[test]
    fn an_order_nothing_above_uses_is_no_order() {
        // `truncate_useless_pathkeys`: the same merge join, its order kept
        // for a clause still to come and dropped when none is.
        let mut kept = JoinRel::new(false);
        kept.add_paths_on(0, 1, &plain(), &plain(), false, true);
        assert!(kept.cheapest_keyed_on(1).is_some());
        let mut dropped = JoinRel::new(false);
        dropped.add_paths_on(0, USELESS_KEYS, &plain(), &plain(), false, true);
        assert!(dropped.paths.iter().all(|p| !p.keyed));
        assert_eq!(
            dropped.cheapest(None, 20808.0).map(|c| c.total),
            kept.cheapest(None, 20808.0).map(|c| c.total)
        );
    }

    #[test]
    fn the_plans_postgres_picks_under_an_order_by() {
        // Two primary keys: a Merge Join in key order on top.
        assert!(ordered_join_on_top(&pk(), &pk(), false, true, 1.0));
        assert!(ordered_join_on_top(&pk(), &pk(), true, true, 1.0));
        // One side keyed: a Hash Join with a Sort on top.
        assert!(!ordered_join_on_top(&pk(), &plain(), false, true, 1.0));
        // A cursor plans for a tenth of the rows: there the merge join over
        // the key's index and a Sort of the other side starts sooner.
        assert!(ordered_join_on_top(&pk(), &plain(), false, true, 0.1));
        // Neither: a Merge Join over two Sorts.
        assert!(ordered_join_on_top(&plain(), &plain(), false, true, 1.0));
    }

    #[test]
    fn a_nested_loop_over_a_unique_parameterized_inner() {
        // PostgreSQL 15.19, three 20-row tables never analysed, lm1.a and
        // lm2.a primary keys: `lm0 join lm1 on lm0.b = lm1.a` is a Nested
        // Loop (0.15..425.16) of lm0's Seq Scan (0.00..30.40, 2,040 rows)
        // and lm1's Index Scan parameterized by lm0.b (0.15..0.19, 1 row),
        // the inner unique: each outer row's scan stops at its match.
        let outer = InPath::scan(0.0, 30.40, 2040.0, None);
        let inner = InPath {
            rows: 1.0,
            req_outer: 1,
            indexed: true,
            ..InPath::scan(0.1525, 0.193_529_4, 1.0, None)
        };
        let sel = 1.0 / 2040.0;
        let (s, t) = nestloop_cost(&outer, &inner, Some((sel, 2040.0)), true, 0.0);
        assert!(close(s, 0.15) && (t - 425.16).abs() < 0.02, "{s} {t}");
    }

    #[test]
    fn a_memoize_over_a_parameterized_inner() {
        // PostgreSQL 15.19, the same tables with plain indexes on a: a
        // Nested Loop (1.60..62.42) of a Hash Join (1.45..41.54, 204 rows)
        // and a Memoize (0.15..0.77) of lm2's Index Only Scan (0.14..0.76,
        // 1 row) keyed on lm1.b (20 values in 20 rows). The inputs are as
        // EXPLAIN rounds them.
        let outer = InPath::scan(1.45, 41.54, 204.0, None);
        let inner = InPath {
            req_outer: 2,
            indexed: true,
            ..InPath::scan(0.14, 0.76, 1.0, None)
        };
        let key = MemoKey {
            ndistinct: 20.0,
            default: false,
            tuples: 20.0,
            rel_rows: 20.0,
            width: 4,
        };
        let m = memoize(&inner, &key, 204.0, 4);
        assert!(close(m.startup, 0.15) && close(m.total, 0.77), "{m:?}");
        let (s, t) = nestloop_cost(&outer, &m, None, false, 0.0);
        assert!(close(s, 1.60) && (t - 62.42).abs() < 0.15, "{s} {t}");
    }

    #[test]
    fn a_limit_takes_the_path_its_rows_cost_least() {
        // `lm0 join lm1 join lm2 limit 5`, 212,242 rows: the nested loop
        // over Materialize (0.00..699250.20) under a Limit costs 16.47,
        // beside the hash joins' startup.
        let nest = JoinChoice {
            combo: 0,
            method: JoinMethod::Other,
            startup: 0.0,
            total: 699_250.20,
            class: None,
            a_path: 0,
            b_path: 0,
            rows: 212_242.0,
            rescan: (0.0, 699_250.20),
            req_outer: 0,
        };
        let hash = JoinChoice {
            startup: 120.0,
            total: 5000.0,
            method: JoinMethod::HashOuterA,
            ..nest
        };
        assert_eq!(limit_choice(&[hash, nest], 212_242.0, 5.0, 0.0), Some(1));
        assert_eq!(
            limit_choice(&[hash, nest], 212_242.0, 50_000.0, 0.0),
            Some(0)
        );
    }

    #[test]
    fn a_sort_under_a_limit_is_bounded() {
        // 2,040 rows of 8 bytes under LIMIT 5: a bounded heap,
        // N log2 2K comparisons.
        let (s, t) = cost_sort(30.40, 2040.0, 8, Some(5.0));
        let expect = 30.40 + 2.0 * CPU_OPERATOR_COST * 2040.0 * 10f64.log2();
        assert!(close(s, expect) && close(t, expect + CPU_OPERATOR_COST * 2040.0));
        // Whole: a quicksort, 142.54 as before.
        let (s, _) = cost_sort(30.40, 2040.0, 8, None);
        assert!(close(s, 142.54), "{s}");
    }

    #[test]
    fn a_join_parameterized_by_an_outer_table_is_kept_where_one_is_needed() {
        // `lm0 LEFT JOIN (lm1 LEFT JOIN lm2 ...)`: the relation {lm1, lm2}
        // may be parameterized by lm0 (bit 1), which the outer join keeps
        // from joining lm1 alone first (`param_source_rels`). lm1 has a
        // scan parameterized by lm0 of 2 rows; lm2 a sequential scan.
        let lm1 = [
            InPath::scan(0.0, 30.40, 2040.0, None),
            InPath {
                req_outer: 1,
                indexed: true,
                ..InPath::scan(0.15, 0.19, 2.0, None)
            },
        ];
        let lm2 = [InPath::scan(0.0, 30.40, 2040.0, None)];
        let side = JoinSide {
            rows: 2040.0,
            tuples: 2040.0,
            rel_rows: 2040.0,
            width: Some(8),
            ..plain()
        };
        let rel = |paths: &'static [InPath], set: u32| RelPaths {
            side,
            paths,
            cheapest_total: 0,
            cheapest_startup: 0,
            set,
            width: 8,
            unique: false,
            key_table: set,
        };
        let lm1: &'static [InPath] = Box::leak(Box::new(lm1));
        let lm2: &'static [InPath] = Box::leak(Box::new(lm2));
        let (a, b) = (rel(lm1, 2), rel(lm2, 4));
        let rows = join_rows(&side, &side, true);
        let mut kept = JoinRel::new(true);
        kept.add_paths_general(0, Some(2), Some(2), &|_| false, &a, &b, true, rows, 1);
        // The parameterized paths' rows: lm1's 2 through the LEFT join's
        // clause, at least the preserved side's (`calc_joinrel_size_estimate`).
        let param: Vec<JoinChoice> = kept
            .pathlist()
            .into_iter()
            .filter(|p| p.req_outer == 1)
            .collect();
        assert!(!param.is_empty());
        assert!(param.iter().all(|p| p.rows == 20.0), "{param:?}");
        // With no outer join to need it, none is made.
        let mut none = JoinRel::new(true);
        none.add_paths_general(0, Some(2), Some(2), &|_| false, &a, &b, true, rows, 0);
        assert!(none.pathlist().iter().all(|p| p.req_outer == 0));
    }
}
