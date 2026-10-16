//! PostgreSQL 15's cost model (`costsize.c`, `selfuncs.c`, `pathnode.c`)
//! for the one choice this server copies: whether a grouping is planned as a
//! HashAggregate or as a Sort followed by a Group, which decides the order
//! the groups come out in (RP-028).
//!
//! The planner prices both over the cheapest way to read the table -- a
//! sequential scan, a bitmap heap scan or an index scan, for a table with no
//! statistics -- and `add_path` keeps the sorted one when the two cost the
//! same within 1% (`STD_FUZZ_FACTOR`), because its output is ordered. Every
//! constant is PostgreSQL's default GUC; every formula was checked against
//! `EXPLAIN` on PostgreSQL 15.19 (the tests below).

use bson::{Bson, Document};

const SEQ_PAGE_COST: f64 = 1.0;
const RANDOM_PAGE_COST: f64 = 4.0;
const CPU_TUPLE_COST: f64 = 0.01;
const CPU_INDEX_TUPLE_COST: f64 = 0.005;
const CPU_OPERATOR_COST: f64 = 0.0025;
const STD_FUZZ_FACTOR: f64 = 1.01;
/// `DEFAULT_PAGE_CPU_MULTIPLIER`: a B-tree page descended, in operators.
const PAGE_CPU_MULTIPLIER: f64 = 50.0;

/// `clamp_row_est`: at least 1, otherwise rounded.
pub fn clamp_row_est(n: f64) -> f64 {
    if n <= 1.0 {
        1.0
    } else {
        n.round()
    }
}

/// One top-level conjunct of a WHERE, on one column.
#[derive(Debug, Clone, PartialEq)]
pub enum Qual {
    /// `col = const`.
    Eq,
    /// `col <> const`: not an index qual.
    Ne,
    /// `col > const`, `>=`, `<`, `<=`; `true` for a lower bound.
    Range(bool),
    /// `col IN (...)` of this many non-NULL values (a `ScalarArrayOpExpr`).
    In(usize),
}

/// A WHERE's conjuncts as `(stored field, qual)`, in no particular order.
/// `None` when it holds anything else -- an `OR`, `IS NULL`, a pattern --
/// whose paths (a BitmapOr) or costs are not modelled.
pub fn conjuncts(filter: &Document) -> Option<Vec<(String, Qual)>> {
    let mut out = Vec::new();
    let mut docs = vec![filter];
    while let Some(doc) = docs.pop() {
        for (key, value) in doc {
            match (key.as_str(), value) {
                ("$and", Bson::Array(items)) => {
                    for item in items {
                        docs.push(item.as_document()?);
                    }
                }
                (k, _) if k.starts_with('$') => return None,
                (_, Bson::Null) => return None,
                (field, Bson::Document(ops)) if ops.keys().any(|k| k.starts_with('$')) => {
                    for (op, operand) in ops {
                        // `<> x` is lowered with `$ne: null` beside it, as
                        // a comparison never holds for a NULL: no qual.
                        if op == "$ne" && operand == &Bson::Null {
                            continue;
                        }
                        if operand == &Bson::Null {
                            return None;
                        }
                        let q = match op.as_str() {
                            "$eq" => Qual::Eq,
                            "$ne" => Qual::Ne,
                            "$gt" | "$gte" => Qual::Range(true),
                            "$lt" | "$lte" => Qual::Range(false),
                            "$in" => Qual::In(
                                operand
                                    .as_array()?
                                    .iter()
                                    .filter(|v| **v != Bson::Null)
                                    .count(),
                            ),
                            _ => return None,
                        };
                        out.push((field.to_string(), q));
                    }
                }
                (field, _) => out.push((field.to_string(), Qual::Eq)),
            }
        }
    }
    Some(out)
}

/// `cost_qual_eval` per row of these quals: one operator each, and an
/// `IN` list half its length (`ScalarArrayOpExpr`'s estimate).
fn qual_cost<'a>(quals: impl Iterator<Item = &'a Qual>) -> f64 {
    quals
        .map(|q| match q {
            Qual::In(n) => CPU_OPERATOR_COST * *n as f64 * 0.5,
            _ => CPU_OPERATOR_COST,
        })
        .sum()
}

/// The selectivity of one column's quals (`clauselist_selectivity` with no
/// statistics): `1 / ndistinct` an equality, its complement `<>`, the sum
/// over an `IN` list, a third one bound of a range and 0.005 both.
fn column_selectivity<'a>(quals: impl Iterator<Item = &'a Qual>, ndistinct: f64) -> f64 {
    let eq = 1.0 / ndistinct.max(1.0);
    let (mut lower, mut upper, mut sel) = (false, false, 1.0);
    for q in quals {
        match q {
            Qual::Eq => sel *= eq,
            Qual::Ne => sel *= 1.0 - eq,
            Qual::In(n) => sel *= (eq * *n as f64).min(1.0),
            Qual::Range(true) => lower = true,
            Qual::Range(false) => upper = true,
        }
    }
    match (lower, upper) {
        (true, true) => sel * 0.005,
        (false, false) => sel,
        _ => sel / 3.0,
    }
}

/// A B-tree index's pages and height (the root's level) as PostgreSQL
/// would have built it from `entries` keys inserted in key order, each of
/// `key_width` bytes: a leaf page holds `8152 / item` items, split at 90%
/// when inserted in order, and one internal level over more than one leaf.
/// The metapage is counted. Exact for the small tables a test makes; a
/// table inserted out of key order has PostgreSQL's leaves fuller of splits
/// than this.
pub fn btree_size(entries: u64, key_width: i64) -> (f64, u32) {
    let item = ((8 + key_width.max(0) + 7) / 8 * 8 + 4) as u64;
    let usable = 8192 - 24 - 16;
    let per_leaf = usable / item;
    if entries <= per_leaf {
        return (2.0, 0);
    }
    let per_split = (usable * 9 / 10) / item;
    let leaves = entries.div_ceil(per_split.max(1));
    let mut pages = 1 + leaves;
    let mut level = leaves;
    let mut height = 0;
    while level > 1 {
        level = level.div_ceil(per_leaf.max(2));
        pages += level;
        height += 1;
    }
    (pages as f64, height)
}

/// What the planner knows of a table with no statistics.
#[derive(Debug, Clone, Copy)]
pub struct Rel {
    /// `baserel->pages`: at least 10 until it is vacuumed.
    pub pages: f64,
    /// `baserel->tuples`: the pages at the rows' density.
    pub tuples: f64,
}

/// A B-tree index whose leading column is `field`.
#[derive(Debug, Clone)]
pub struct Index {
    pub field: String,
    /// Unique on that column alone.
    pub unique: bool,
    pub pages: f64,
    pub height: u32,
    /// Its key columns, in order (the first is `field`).
    pub columns: Vec<String>,
}

/// `index_pages_fetched` (Mackert and Lohman) for one table that fits
/// `effective_cache_size`: `2TN / (2T + N)`, at most `T`, rounded up.
fn index_pages_fetched(tuples_fetched: f64, pages: f64) -> f64 {
    let t = if pages > 1.0 { pages } else { 1.0 };
    let fetched = 2.0 * t * tuples_fetched / (2.0 * t + tuples_fetched);
    if fetched >= t {
        t
    } else {
        fetched.ceil()
    }
}

/// An index path's cost before the heap: `(startup, total, selectivity)`
/// (`btcostestimate` over `genericcostestimate`), or `None` when no qual
/// can be an index qual -- unless `full`, a scan of the whole index for its
/// order.
fn index_cost(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    ndistinct: f64,
    full: bool,
) -> Option<(f64, f64, f64)> {
    let iq: Vec<&Qual> = quals
        .iter()
        .filter(|(f, q)| f == &index.field && !matches!(q, Qual::Ne))
        .map(|(_, q)| q)
        .collect();
    if iq.is_empty() && !full {
        return None;
    }
    let sel = column_selectivity(iq.iter().copied(), ndistinct);
    let mut num_sa = 1.0;
    for q in &iq {
        if let Qual::In(n) = q {
            if *n > 1 {
                num_sa *= *n as f64;
            }
        }
    }
    let saop = iq.iter().any(|q| matches!(q, Qual::In(_)));
    let unique_eq = index.unique && !saop && iq.iter().any(|q| matches!(q, Qual::Eq));
    let mut nit = if unique_eq {
        1.0
    } else {
        (sel * rel.tuples / num_sa).round()
    };
    nit = nit.min(rel.tuples).max(1.0);
    let index_pages = if index.pages > 1.0 && rel.tuples > 1.0 {
        (nit * index.pages / rel.tuples).ceil()
    } else {
        1.0
    };
    let mut total = if num_sa > 1.0 {
        index_pages_fetched(index_pages * num_sa, index.pages) * RANDOM_PAGE_COST
    } else {
        index_pages * RANDOM_PAGE_COST
    };
    let qual_op = CPU_OPERATOR_COST * iq.len() as f64;
    total += nit * num_sa * (CPU_INDEX_TUPLE_COST + qual_op);
    let mut startup = 0.0;
    if rel.tuples > 1.0 {
        let descent = rel.tuples.log2().ceil() * CPU_OPERATOR_COST;
        startup += descent;
        total += descent * num_sa;
    }
    let descent = f64::from(index.height + 1) * PAGE_CPU_MULTIPLIER * CPU_OPERATOR_COST;
    startup += descent;
    total += descent * num_sa;
    Some((startup, total, sel))
}

/// `cost_bitmap_heap_scan` over one index: `(startup, total)`. `rows` is
/// the relation's estimated output, which the bitmap charges for.
fn bitmap_heap_cost(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    ndistinct: f64,
    rows: f64,
) -> Option<(f64, f64)> {
    let (_, index_total, sel) = index_cost(rel, index, quals, ndistinct, false)?;
    let startup = index_total + 0.1 * CPU_OPERATOR_COST * rows;
    let tuples_fetched = clamp_row_est(sel * rel.tuples);
    let t = if rel.pages > 1.0 { rel.pages } else { 1.0 };
    let pages = index_pages_fetched(tuples_fetched, rel.pages);
    let per_page = if pages >= 2.0 {
        RANDOM_PAGE_COST - (RANDOM_PAGE_COST - SEQ_PAGE_COST) * (pages / t).sqrt()
    } else {
        RANDOM_PAGE_COST
    };
    let cpu = (CPU_TUPLE_COST + qual_cost(quals.iter().map(|(_, q)| q))) * tuples_fetched;
    Some((startup, startup + pages * per_page + cpu))
}

/// `cost_index` for a plain index scan, uncorrelated (a table with no
/// statistics has correlation 0): `(startup, total)`.
fn index_scan_cost(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    ndistinct: f64,
) -> Option<(f64, f64)> {
    index_scan(rel, index, quals, ndistinct, false)
}

/// An index scan read for its order (`build_index_paths` keeps one whose
/// pathkeys are useful): over its index quals, or the whole index when no
/// qual restricts its leading column. `(startup, total)`.
pub fn ordered_index_scan(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    ndistinct: f64,
) -> (f64, f64) {
    index_scan(rel, index, quals, ndistinct, true).unwrap_or((0.0, f64::INFINITY))
}

fn index_scan(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    ndistinct: f64,
    full: bool,
) -> Option<(f64, f64)> {
    let (startup, index_total, sel) = index_cost(rel, index, quals, ndistinct, full)?;
    let tuples_fetched = clamp_row_est(sel * rel.tuples);
    let io = index_pages_fetched(tuples_fetched, rel.pages) * RANDOM_PAGE_COST;
    let other = qual_cost(
        quals
            .iter()
            .filter(|(f, q)| f != &index.field || matches!(q, Qual::Ne))
            .map(|(_, q)| q),
    );
    Some((
        startup,
        index_total + io + (CPU_TUPLE_COST + other) * tuples_fetched,
    ))
}

/// The cheapest total cost of reading `rel` through `quals`: a sequential
/// scan, or a bitmap heap or index scan over an index whose leading column
/// a qual restricts. `ndistinct` gives a column's distinct count; `rows`
/// is the estimated output.
pub fn cheapest_scan(
    rel: &Rel,
    indexes: &[Index],
    quals: &[(String, Qual)],
    ndistinct: &dyn Fn(&str) -> f64,
    rows: f64,
) -> f64 {
    let seq = SEQ_PAGE_COST * rel.pages
        + (CPU_TUPLE_COST + qual_cost(quals.iter().map(|(_, q)| q))) * rel.tuples;
    let mut best = seq;
    for index in indexes {
        let nd = ndistinct(&index.field);
        for cost in [
            bitmap_heap_cost(rel, index, quals, nd, rows).map(|c| c.1),
            index_scan_cost(rel, index, quals, nd).map(|c| c.1),
        ]
        .into_iter()
        .flatten()
        {
            best = best.min(cost);
        }
    }
    best
}

/// The aggregates' costs, which both plans pay (`get_agg_clause_costs`).
#[derive(Debug, Clone, Copy, Default)]
pub struct AggCosts {
    pub trans_per_tuple: f64,
    pub final_per_group: f64,
}

impl AggCosts {
    /// `n` aggregates whose transition functions cost one operator a row.
    pub fn simple(n: usize, finals: usize) -> AggCosts {
        AggCosts {
            trans_per_tuple: CPU_OPERATOR_COST * n as f64,
            final_per_group: CPU_OPERATOR_COST * finals as f64,
        }
    }
}

/// Whether the planner keeps a Sort + Group (a GroupAggregate when `aggs`
/// is `Some`, a Unique for a DISTINCT, which costs as a Group) over a
/// HashAggregate, both over an input costing `input_total` and yielding
/// `rows`, into `groups` groups of `ncols` columns. `add_path` adds the
/// sorted path first; the hashed one is rejected if it costs the same
/// within the fuzz factor (startup cost breaking a total tie), or more;
/// when each is better on one cost both are kept and the cheaper total
/// is the plan -- the hashed one with a Sort over it when `ordered` (the
/// sorted path's order is the ORDER BY's, which the hash must then sort
/// for; `create_ordered_paths`).
pub fn sorted_grouping_wins(
    input_total: f64,
    rows: f64,
    groups: f64,
    ncols: usize,
    aggs: Option<AggCosts>,
    ordered: bool,
) -> bool {
    let (h, s) = grouping_costs(input_total, rows, groups, ncols, aggs);
    // compare_path_costs_fuzzily(hash, sorted); the sorted path has the
    // better pathkeys.
    let f = STD_FUZZ_FACTOR;
    let hash_better = if h.1 > s.1 * f {
        if s.0 > h.0 * f {
            None // different: both kept
        } else {
            Some(false)
        }
    } else if s.1 > h.1 * f {
        if h.0 > s.0 * f {
            None
        } else {
            Some(true)
        }
    } else if h.0 > s.0 * f {
        Some(false)
    } else if s.0 > h.0 * f {
        Some(true)
    } else {
        Some(false) // equal: the sorted path's order wins
    };
    match hash_better {
        Some(false) => true,
        // Each better on something (the hash on cost, the sort on order):
        // both are kept, and the cheaper total is the plan.
        _ => {
            let resort = if ordered {
                let g = groups.max(2.0);
                2.0 * CPU_OPERATOR_COST * g * g.log2() + CPU_OPERATOR_COST * groups
            } else {
                0.0
            };
            s.1 < h.1 + resort
        }
    }
}

/// Whether a grouping under a LIMIT of `limit` rows (OFFSET included) is
/// read sorted: the planner keeps, besides the HashAggregate and the Sort +
/// Group over the cheapest scan, a Group over each scan already in the
/// group keys' order (`presorted`, `(startup, total)`), and the LIMIT takes
/// the one cheapest to its first rows -- `startup + (total - startup) *
/// limit / groups` (`create_limit_path`, `get_cheapest_fractional_path`).
pub fn limited_grouping_sorted(
    input_total: f64,
    rows: f64,
    groups: f64,
    ncols: usize,
    aggs: Option<AggCosts>,
    limit: f64,
    presorted: &[(f64, f64, f64)],
) -> bool {
    let (h, s) = grouping_costs(input_total, rows, groups, ncols, aggs);
    let fraction = (limit / groups.max(1.0)).min(1.0);
    let first = |(startup, total): (f64, f64)| startup + (total - startup) * fraction;
    let hash = first(h);
    let mut best_sorted = first(s);
    for &(startup, total, input_rows) in presorted {
        let a = aggs.unwrap_or_default();
        let cols = CPU_OPERATOR_COST * ncols as f64 * input_rows;
        let total = total
            + cols
            + match aggs {
                Some(_) => {
                    a.trans_per_tuple * input_rows
                        + a.final_per_group * groups
                        + CPU_TUPLE_COST * groups
                }
                None => 0.0,
            };
        best_sorted = best_sorted.min(first((startup, total)));
    }
    best_sorted < hash
}

/// The HashAggregate's and the Sort + Group's `(startup, total)`.
fn grouping_costs(
    input_total: f64,
    rows: f64,
    groups: f64,
    ncols: usize,
    aggs: Option<AggCosts>,
) -> ((f64, f64), (f64, f64)) {
    let a = aggs.unwrap_or_default();
    let cols = CPU_OPERATOR_COST * ncols as f64 * rows;
    let hash_startup = input_total + a.trans_per_tuple * rows + cols;
    let hash_total = hash_startup + a.final_per_group * groups + CPU_TUPLE_COST * groups;
    let n = rows.max(2.0);
    let sort_startup = input_total + 2.0 * CPU_OPERATOR_COST * n * n.log2();
    let sort_total = sort_startup + CPU_OPERATOR_COST * rows;
    let group_total = sort_total
        + cols
        + match aggs {
            Some(a) => {
                a.trans_per_tuple * rows + a.final_per_group * groups + CPU_TUPLE_COST * groups
            }
            None => 0.0,
        };
    ((hash_startup, hash_total), (sort_startup, group_total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 0.006
    }

    /// PostgreSQL 15.19, fresh tables of 40 rows: t2 (k int primary key,
    /// g int) is 10 pages of 226 rows, t3 (k int primary key, g int, t
    /// text) 10 of 120. The index on k is 2 pages, height 0.
    fn t2() -> Rel {
        Rel {
            pages: 10.0,
            tuples: 2260.0,
        }
    }
    fn t3() -> Rel {
        Rel {
            pages: 10.0,
            tuples: 1200.0,
        }
    }
    fn pkey() -> Index {
        Index {
            field: "k".into(),
            unique: true,
            pages: 2.0,
            height: 0,
            columns: vec!["k".into()],
        }
    }
    fn q(filter: Document) -> Vec<(String, Qual)> {
        conjuncts(&filter).unwrap()
    }

    #[test]
    fn index_and_bitmap_scans_cost_as_postgres_does() {
        let range = q(bson::doc! {"k": {"$gt": 5, "$lt": 30}});
        // Bitmap Index Scan (0.00..4.21 rows=6), Bitmap Heap Scan
        // (4.21..13.70 rows=6) over t3.
        let (_, it, _) = index_cost(&t3(), &pkey(), &range, 1200.0, false).unwrap();
        assert!(close(it, 4.21), "{it}");
        let (s, t) = bitmap_heap_cost(&t3(), &pkey(), &range, 1200.0, 6.0).unwrap();
        assert!(close(s, 4.21) && close(t, 13.70), "{s} {t}");
        // t2: Bitmap Heap Scan (4.27..14.97 rows=11).
        let (s, t) = bitmap_heap_cost(&t2(), &pkey(), &range, 2260.0, 11.0).unwrap();
        assert!(close(s, 4.27) && close(t, 14.97), "{s} {t}");
        // `k > 5`: Bitmap Index Scan 9.80, Heap (9.99..29.40 rows=753).
        let gt = q(bson::doc! {"k": {"$gt": 5}});
        let (s, t) = bitmap_heap_cost(&t2(), &pkey(), &gt, 2260.0, 753.0).unwrap();
        assert!(close(s, 9.99) && close(t, 29.40), "{s} {t}");
        // `k = 3`: Index Scan (0.15..8.17 rows=1).
        let eq = q(bson::doc! {"k": 3});
        let (s, t) = index_scan_cost(&t2(), &pkey(), &eq, 2260.0).unwrap();
        assert!(close(s, 0.15) && close(t, 8.17), "{s} {t}");
        // `k in (3, 5, 7)`: Bitmap Index Scan 8.49, Heap (8.49..15.60).
        let saop = q(bson::doc! {"k": {"$in": [3, 5, 7]}});
        let (s, t) = bitmap_heap_cost(&t2(), &pkey(), &saop, 2260.0, 3.0).unwrap();
        assert!(close(s, 8.49) && close(t, 15.60), "{s} {t}");
        // A `<>` beside them is rechecked on the heap: (4.27..14.99).
        let ne = q(bson::doc! {"k": {"$gt": 5, "$lt": 30}, "g": {"$ne": 2}});
        let (_, t) = bitmap_heap_cost(&t2(), &pkey(), &ne, 2260.0, 11.0).unwrap();
        assert!(close(t, 14.99), "{t}");
        // Seq Scan with `g = 1` (0.00..38.25), without a qual 32.60.
        let nd = |_: &str| 200.0;
        let geq = q(bson::doc! {"g": 1});
        assert!(close(
            cheapest_scan(&t2(), &[pkey()], &geq, &nd, 11.0),
            38.25
        ));
        assert!(close(
            cheapest_scan(&t2(), &[pkey()], &[], &nd, 2260.0),
            32.60
        ));
    }

    #[test]
    fn sort_and_group_wins_where_postgres_plans_it() {
        // t3 `k > 5 and k < 30 group by g`: 6 rows into 6 groups, Sort +
        // Group (13.77..13.80).
        assert!(sorted_grouping_wins(13.70, 6.0, 6.0, 1, None, false));
        // t2 the same, 11 rows into 11 groups: HashAggregate.
        assert!(!sorted_grouping_wins(14.97, 11.0, 11.0, 1, None, false));
        // `k in (3, 5, 7) group by k`: Sort + Group.
        assert!(sorted_grouping_wins(15.60, 3.0, 3.0, 1, None, false));
        // A whole t2 into 200 groups: HashAggregate.
        assert!(!sorted_grouping_wins(32.60, 2260.0, 200.0, 1, None, false));
        // tb `group by g, b` over 11 rows into 11 groups: Sort + Group.
        assert!(sorted_grouping_wins(14.97, 11.0, 11.0, 2, None, false));
    }

    #[test]
    fn a_limit_reads_an_index_in_its_order() {
        let nd = |_: &str| 2260.0;
        // `select k from t2 group by k limit 5`: Index Only Scan
        // (0.15..82.06), Group (0.15..87.71), Limit (0.15..0.35).
        let (s, t) = ordered_index_scan(&t2(), &pkey(), &[], nd("k"));
        assert!(close(s, 0.15) && close(t, 82.06), "{s} {t}");
        assert!(limited_grouping_sorted(
            32.60,
            2260.0,
            2260.0,
            1,
            None,
            5.0,
            &[(s, t, 2260.0)]
        ));
        // `where k > 5 and k < 30 ... limit 5` over 40 rows: HashAggregate
        // over the bitmap heap scan (Limit 14.99..15.04).
        let range = q(bson::doc! {"k": {"$gt": 5, "$lt": 30}});
        let (s, t) = ordered_index_scan(&t2(), &pkey(), &range, nd("k"));
        assert!(!limited_grouping_sorted(
            14.97,
            11.0,
            11.0,
            1,
            None,
            5.0,
            &[(s, t, 11.0)]
        ));
        // Over 3,000 rows (14 pages, 3,164 tuples, an 11-page index of
        // height 1): Group over the Index Only Scan (0.28..48.60).
        let rel = Rel {
            pages: 14.0,
            tuples: 3164.0,
        };
        let ix = Index {
            pages: 11.0,
            height: 1,
            ..pkey()
        };
        let (s, t) = ordered_index_scan(&rel, &ix, &range, nd("k"));
        assert!(close(s, 0.28) && close(t, 48.60), "{s} {t}");
        let input = cheapest_scan(&rel, &[ix], &range, &nd, 16.0);
        assert!(close(input, 19.43), "{input}");
        assert!(limited_grouping_sorted(
            input,
            16.0,
            16.0,
            1,
            None,
            5.0,
            &[(s, t, 16.0)]
        ));
    }

    #[test]
    fn btree_sizes() {
        assert_eq!(btree_size(40, 4), (2.0, 0));
        assert_eq!(btree_size(407, 4), (2.0, 0));
        assert_eq!(btree_size(1000, 4).1, 1);
    }
}
