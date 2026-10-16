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

/// A nested loop rescanning `inner` for each outer row, the inner read
/// whole each time or, when `materialized`, once into a Material node
/// (`cost_material`: two operator costs a tuple, in memory) and rescanned
/// at one operator cost a tuple (`cost_rescan`). `initial_cost_nestloop` /
/// `final_cost_nestloop`: the join clause is evaluated for every pair.
/// `(startup, total)`.
pub fn nest_loop(outer: &JoinSide, inner: &JoinSide, materialized: bool) -> (f64, f64) {
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
    run += (CPU_TUPLE_COST + CPU_OPERATOR_COST) * outer.rows * inner.rows;
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
    /// An inner join with `b` outer: a hash join probing with `b`'s rows in
    /// scan order, `a` hashed.
    HashOuterB,
    /// An inner join with `b` outer: a merge join, each side in key order.
    MergeOuterB { a_by_index: bool, b_by_index: bool },
    /// An inner join with `b` outer: a nested loop, both sides in scan
    /// order.
    NestOuterB,
    /// Any other: a LEFT join with `b` outer (a hash or merge right join),
    /// or a plan whose rows come in both sides' scan order, `a` outer: a
    /// nested loop, or a hash join on a unique `b`.
    Other,
}

/// One path `add_path` weighs: its `(startup, total)`, whether it comes
/// out in the join key's order (a merge join's pathkeys), and its plan.
#[derive(Clone, Copy)]
struct JoinPath {
    startup: f64,
    total: f64,
    keyed: bool,
    method: JoinMethod,
}

/// `compare_path_costs_fuzzily`: which of `p1` and `p2` is cheaper in
/// total by more than `fuzz` (or, fuzzily equal there, in startup); when
/// startup costs count, a path cheaper in total but dearer to start is
/// `Different`, and both are kept.
fn fuzzy_cmp(p1: &JoinPath, p2: &JoinPath, fuzz: f64, startup_counts: bool) -> Ordering3 {
    if p1.total > p2.total * fuzz {
        if startup_counts && p2.startup > p1.startup * fuzz {
            return Ordering3::Different;
        }
        return Ordering3::Better2;
    }
    if p2.total > p1.total * fuzz {
        if startup_counts && p1.startup > p2.startup * fuzz {
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

/// `add_path`: `new` joins the paths unless one there is as cheap (1%
/// fuzz) and as well ordered, and pushes out any it beats on both. All the
/// paths of one join have the same rows and parameterization.
fn add_path(paths: &mut Vec<JoinPath>, new: JoinPath, startup_counts: bool) {
    let mut accept = true;
    paths.retain(|old| {
        if !accept {
            return true;
        }
        let costs = fuzzy_cmp(&new, old, 1.01, startup_counts);
        if costs == Ordering3::Different {
            return true;
        }
        // Pathkeys: a keyed path's are better than none.
        let keys = match (new.keyed, old.keyed) {
            (true, false) => Ordering3::Better1,
            (false, true) => Ordering3::Better2,
            _ => Ordering3::Equal,
        };
        let mut remove_old = false;
        match costs {
            Ordering3::Equal => match keys {
                Ordering3::Better1 => remove_old = true,
                Ordering3::Better2 => accept = false,
                _ => {
                    if fuzzy_cmp(&new, old, 1.000_000_000_1, startup_counts) == Ordering3::Better1 {
                        remove_old = true;
                    } else {
                        accept = false;
                    }
                }
            },
            Ordering3::Better1 if keys != Ordering3::Better2 => remove_old = true,
            Ordering3::Better2 if keys != Ordering3::Better1 => accept = false,
            _ => {}
        }
        !remove_old
    });
    if accept {
        paths.push(new);
    }
}

/// `JoinMethod` for `a JOIN b` (`left`: a LEFT join, `a` its outer side).
/// `limit` is a LIMIT's rows (with its OFFSET), for which the plan is the
/// cheapest for that fraction of the join's rows
/// (`get_cheapest_fractional_path`); `None` for the whole result.
pub fn cheapest_join(a: &JoinSide, b: &JoinSide, left: bool, limit: Option<f64>) -> JoinMethod {
    // A join rel weighs startup costs only under a LIMIT
    // (`consider_startup`: a positive `tuple_fraction`).
    let startup_counts = limit.is_some();
    let mut paths: Vec<JoinPath> = Vec::new();
    let mut consider = |(startup, total): (f64, f64), keyed: bool, method: JoinMethod| {
        add_path(
            &mut paths,
            JoinPath {
                startup,
                total,
                keyed,
                method,
            },
            startup_counts,
        );
    };
    // `populate_joinrel_with_paths` builds `a` outer first, then `b`; each
    // `add_paths_to_joinrel` in turn tries merge joins over two Sorts
    // (`sort_inner_and_outer`), then nested loops and merge joins over an
    // outer already in order (`match_unsorted_outer`), then hash joins.
    // A LEFT join with `b` outer is a right join, whose unmatched `a` rows
    // come last, in the hash table's order: not modelled. A nested loop
    // with `a` outer emits its rows in both sides' scan order.
    for b_outer in [false, true] {
        let (outer, inner) = if b_outer { (b, a) } else { (a, b) };
        let pick = |m: JoinMethod| {
            if b_outer && left {
                JoinMethod::Other
            } else {
                m
            }
        };
        let merge = |oi: bool, ii: bool| {
            if b_outer {
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
        if let Some(cost) = merge_join(outer, inner, false, false) {
            consider(cost, true, pick(merge(false, false)));
        }
        let nest = if b_outer {
            JoinMethod::NestOuterB
        } else {
            JoinMethod::Other
        };
        // Under a LIMIT the outer's cheapest scan to start is a path of its
        // own, which nested loops and hash joins read too
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
        if !(b_outer && left) {
            for o in &outers {
                consider(nest_loop(o, inner, false), false, nest);
                consider(nest_loop(o, inner, true), false, nest);
            }
        }
        for (oi, ii) in [(true, false), (true, true)] {
            if let Some(cost) = merge_join(outer, inner, oi, ii) {
                consider(cost, true, pick(merge(oi, ii)));
            }
        }
        let hash = if b_outer {
            JoinMethod::HashOuterB
        } else if b.unique {
            JoinMethod::Other
        } else {
            JoinMethod::HashOuterA
        };
        for o in &outers {
            consider(hash_join(o, inner), false, pick(hash));
        }
    }
    // `set_cheapest`: the cheapest in total, a tie to the cheaper startup;
    // under a LIMIT, any other path cheaper for its fraction of the rows.
    let total_cmp = |p: &JoinPath, q: &JoinPath| {
        p.total
            .total_cmp(&q.total)
            .then(p.startup.total_cmp(&q.startup))
    };
    let mut best = *paths
        .iter()
        .min_by(|p, q| total_cmp(p, q))
        .expect("a join has paths");
    if let Some(limit) = limit {
        let rows = join_rows(a, b, left);
        let fraction = if limit >= 1.0 && rows > 0.0 {
            limit / rows
        } else {
            limit
        };
        if fraction > 0.0 && fraction < 1.0 {
            let price = |p: &JoinPath| p.startup + fraction * (p.total - p.startup);
            for p in &paths {
                if price(p) < price(&best) {
                    best = *p;
                }
            }
        }
    }
    best.method
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
            scan: (9.99, 29.40),
            startup_scan: (0.0, 38.25),
            ..plain()
        };
        let jq = JoinSide {
            rows: 753.0,
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
