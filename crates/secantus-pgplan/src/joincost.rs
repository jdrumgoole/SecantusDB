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
    /// An index scan in the join key's order, if an index leads with it.
    pub ordered: Option<(f64, f64)>,
    /// The join key is unique (a one-column primary key or unique index).
    pub unique: bool,
    /// The join key's distinct count (`get_variable_numdistinct`), and
    /// whether it is the 200 default.
    pub ndistinct: f64,
    pub default_ndistinct: bool,
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

/// `eqjoinsel` with no statistics: `1 / max(nd1, nd2)`. A plain or LEFT
/// join's semi-join factors use this too (the special join info's type is
/// not SEMI).
fn join_selectivity(outer: &JoinSide, inner: &JoinSide) -> f64 {
    1.0 / outer.ndistinct.max(inner.ndistinct).max(1.0)
}

/// The join's estimated rows (`calc_joinrel_size_estimate`); a LEFT join
/// keeps at least every outer row.
pub fn join_rows(outer: &JoinSide, inner: &JoinSide, left: bool) -> f64 {
    let n = outer.rows * inner.rows * join_selectivity(outer, inner);
    clamp_row_est(if left { n.max(outer.rows) } else { n })
}

/// A hash join with `inner` hashed: `(startup, total)`.
pub fn hash_join(outer: &JoinSide, inner: &JoinSide) -> (f64, f64) {
    let mut startup = outer.scan.0 + inner.scan.1;
    let mut run = outer.scan.1 - outer.scan.0;
    startup += (CPU_OPERATOR_COST + CPU_TUPLE_COST) * inner.rows;
    run += CPU_OPERATOR_COST * outer.rows;
    // ExecChooseHashTableSize: a power of two at least the rows, at least
    // 1,024; one batch for the tables this models.
    let buckets = (inner.rows.max(1.0).ceil() as u64)
        .next_power_of_two()
        .max(1024) as f64;
    // estimate_hash_bucket_stats: 0.1 for a default distinct count.
    let bucketsize = if inner.default_ndistinct {
        0.1
    } else {
        let mut nd = inner.ndistinct;
        if inner.tuples > 0.0 {
            nd = clamp_row_est(nd * inner.rows / inner.tuples);
        }
        let f = if nd > buckets {
            1.0 / buckets
        } else {
            1.0 / nd
        };
        f.clamp(1.0e-6, 1.0)
    };
    let sel = join_selectivity(outer, inner);
    let tuples = if inner.unique {
        let match_frac = sel;
        let match_count = (sel * inner.rows / sel).max(1.0);
        let matched = (outer.rows * match_frac).round();
        let scan_frac = 2.0 / (match_count + 1.0);
        run +=
            CPU_OPERATOR_COST * matched * clamp_row_est(inner.rows * bucketsize * scan_frac) * 0.5;
        run +=
            CPU_OPERATOR_COST * (outer.rows - matched) * clamp_row_est(inner.rows / buckets) * 0.05;
        matched
    } else {
        run += CPU_OPERATOR_COST * outer.rows * clamp_row_est(inner.rows * bucketsize) * 0.5;
        clamp_row_est(outer.rows * inner.rows * sel)
    };
    run += CPU_TUPLE_COST * tuples;
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
    run += CPU_OPERATOR_COST * (outer.rows + inner.rows * ratio);
    run += CPU_TUPLE_COST * tuples;
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
    /// Any other: `b` outer (a hash or merge join the other way round).
    Other,
}

/// `JoinMethod` for `a JOIN b` (`left`: a LEFT join, `a` its outer side).
pub fn cheapest_join(a: &JoinSide, b: &JoinSide, left: bool) -> JoinMethod {
    let _ = left;
    let mut best = (f64::INFINITY, JoinMethod::Other);
    let mut consider = |cost: f64, method: JoinMethod| {
        if cost < best.0 {
            best = (cost, method);
        }
    };
    consider(
        hash_join(a, b).1,
        if b.unique {
            JoinMethod::Other
        } else {
            JoinMethod::HashOuterA
        },
    );
    consider(hash_join(b, a).1, JoinMethod::Other);
    for (oi, ii) in [(false, false), (true, false), (true, true)] {
        if let Some((_, t)) = merge_join(a, b, oi, ii) {
            consider(
                t,
                JoinMethod::MergeOuterA {
                    a_by_index: oi,
                    b_by_index: ii,
                },
            );
        }
        if let Some((_, t)) = merge_join(b, a, oi, ii) {
            consider(t, JoinMethod::Other);
        }
    }
    best.1
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
            ordered: Some((0.155, 82.055)),
            unique: true,
            ndistinct: 2260.0,
            default_ndistinct: false,
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
    }

    #[test]
    fn the_method_postgres_picks_for_a_whole_join() {
        // Two keys: a Hash Join, jp probing (60.85..99.39), one match a row.
        assert_eq!(cheapest_join(&pk(), &pk(), false), JoinMethod::Other);
        // Two plain columns, many to many: a Merge Join over two Sorts.
        assert_eq!(
            cheapest_join(&plain(), &plain(), false),
            JoinMethod::MergeOuterA {
                a_by_index: false,
                b_by_index: false
            }
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
}
