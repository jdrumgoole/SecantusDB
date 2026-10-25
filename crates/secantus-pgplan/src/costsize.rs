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
    /// A clause no index serves here (an `OR`), costing this many operators
    /// a row.
    Filter(f64),
    /// `col IS NULL` (`true`) or `col IS NOT NULL`: a B-tree index qual
    /// (`amsearchnulls`), costing nothing on the heap (`cost_qual_eval`
    /// charges no NullTest).
    IsNull(bool),
    /// `col NOT IN (...)` of this many values (`<> ALL`): never an index
    /// qual.
    NotIn(usize),
}

/// From this many elements a constant `IN` / `NOT IN` list is a hashed
/// ScalarArrayOpExpr (`MIN_ARRAY_SIZE_FOR_HASHED_SAOP`).
const HASHED_SAOP: usize = 9;

impl Qual {
    /// An index qual on its column (`match_clause_to_indexcol`).
    fn indexable(&self) -> bool {
        !matches!(self, Qual::Ne | Qual::Filter(_) | Qual::NotIn(_))
    }
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

/// `cost_qual_eval` per row of these quals: one operator each, an `IN` /
/// `NOT IN` list half its length (`ScalarArrayOpExpr`'s estimate) -- or,
/// hashed, one hash and one comparison -- and a null test nothing.
fn qual_cost<'a>(quals: impl Iterator<Item = &'a Qual>) -> f64 {
    quals
        .map(|q| match q {
            Qual::In(n) | Qual::NotIn(n) if *n >= HASHED_SAOP => 2.0 * CPU_OPERATOR_COST,
            Qual::In(n) | Qual::NotIn(n) => CPU_OPERATOR_COST * *n as f64 * 0.5,
            Qual::Filter(ops) => CPU_OPERATOR_COST * ops,
            Qual::IsNull(_) => 0.0,
            _ => CPU_OPERATOR_COST,
        })
        .sum()
}

/// `cost_qual_eval`'s startup for these quals: a hashed list's table, one
/// hash an element.
fn qual_startup<'a>(quals: impl Iterator<Item = &'a Qual>) -> f64 {
    quals
        .map(|q| match q {
            Qual::In(n) | Qual::NotIn(n) if *n >= HASHED_SAOP => CPU_OPERATOR_COST * *n as f64,
            _ => 0.0,
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
            Qual::Filter(_) | Qual::IsNull(_) | Qual::NotIn(_) => {}
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
    /// `btcostestimate`'s `indexCorrelation`: the leading column's ANALYZE
    /// correlation (three quarters of it for an index of several columns);
    /// 0 with no statistics.
    pub correlation: f64,
    /// For a scan that can be index-only (the index holds every column the
    /// statement reads), the heap's all-visible fraction, whose pages it
    /// does not fetch (`cost_index`); 0 otherwise.
    pub visible: f64,
    /// `IndexOptInfo.tuples` of a partial index, its own `estimate_rel_size`
    /// (at most the relation's); `None` is the relation's tuples.
    pub tuples: Option<f64>,
    /// A partial index whose predicate the query implies (`predOK`): the
    /// selectivity of the predicate's clauses its index quals do not imply
    /// (`add_predicate_to_index_quals`). Such an index serves a scan with
    /// no index qual at all. `None` for a full index.
    pub predicate: Option<f64>,
    /// A hash index (`hashcostestimate`, which is `genericcostestimate`):
    /// only `=` and `IN` serve it, and it has no B-tree descent to cost.
    pub hash: bool,
}

/// How the quals on an index's leading column are estimated: with no
/// statistics, from its distinct count (`column_selectivity`); or the
/// selectivity statistics gave them (`clauselist_selectivity` over them).
#[derive(Debug, Clone, Copy)]
pub enum ColSel {
    Ndistinct(f64),
    Given(f64),
}

impl ColSel {
    fn of(self, quals: &[&Qual]) -> f64 {
        match self {
            ColSel::Ndistinct(nd) => column_selectivity(quals.iter().copied(), nd),
            ColSel::Given(sel) => sel,
        }
    }
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
    colsel: ColSel,
    full: bool,
) -> Option<(f64, f64, f64)> {
    let iq: Vec<&Qual> = quals
        .iter()
        .filter(|(f, q)| {
            f == &index.field
                && if index.hash {
                    matches!(q, Qual::Eq | Qual::In(_))
                } else {
                    q.indexable()
                }
        })
        .map(|(_, q)| q)
        .collect();
    if iq.is_empty() && !full && index.predicate.is_none() {
        return None;
    }
    let sel = colsel.of(&iq) * index.predicate.unwrap_or(1.0);
    let itup = index.tuples.unwrap_or(rel.tuples);
    let mut num_sa = 1.0;
    for q in &iq {
        if let Qual::In(n) = q {
            if *n > 1 {
                num_sa *= *n as f64;
            }
        }
    }
    let saop = iq.iter().any(|q| matches!(q, Qual::In(_)));
    let unique_eq =
        index.unique && !index.hash && !saop && iq.iter().any(|q| matches!(q, Qual::Eq));
    let mut nit = if unique_eq {
        1.0
    } else {
        (sel * rel.tuples / num_sa).round()
    };
    nit = nit.min(itup).max(1.0);
    let index_pages = if index.pages > 1.0 && itup > 1.0 {
        (nit * index.pages / itup).ceil()
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
    if index.hash {
        return Some((startup, total, sel));
    }
    if itup > 1.0 {
        let descent = itup.log2().ceil() * CPU_OPERATOR_COST;
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
    colsel: ColSel,
    rows: f64,
) -> Option<(f64, f64)> {
    let (_, index_total, sel) = index_cost(rel, index, quals, colsel, false)?;
    let startup = index_total + 0.1 * CPU_OPERATOR_COST * rows;
    Some(bitmap_heap_from(rel, startup, sel, quals))
}

/// `cost_bitmap_heap_scan` over a bitmap costing `startup` that selects
/// `sel` of the rows: the heap pages fetched, nearer sequential the more
/// of them, and every qual rechecked on each row.
fn bitmap_heap_from(rel: &Rel, startup: f64, sel: f64, quals: &[(String, Qual)]) -> (f64, f64) {
    let startup = startup + qual_startup(quals.iter().map(|(_, q)| q));
    let tuples_fetched = clamp_row_est(sel * rel.tuples);
    let t = if rel.pages > 1.0 { rel.pages } else { 1.0 };
    let pages = index_pages_fetched(tuples_fetched, rel.pages);
    let per_page = if pages >= 2.0 {
        RANDOM_PAGE_COST - (RANDOM_PAGE_COST - SEQ_PAGE_COST) * (pages / t).sqrt()
    } else {
        RANDOM_PAGE_COST
    };
    let cpu = (CPU_TUPLE_COST + qual_cost(quals.iter().map(|(_, q)| q))) * tuples_fetched;
    (startup, startup + pages * per_page + cpu)
}

/// `cost_index` for a plain index scan: `(startup, total)`.
fn index_scan_cost(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    colsel: ColSel,
) -> Option<(f64, f64)> {
    index_scan(rel, index, quals, colsel, false)
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
    ordered_index_scan_with(rel, index, quals, ColSel::Ndistinct(ndistinct))
}

/// `ordered_index_scan` with its quals estimated by `colsel`.
pub fn ordered_index_scan_with(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    colsel: ColSel,
) -> (f64, f64) {
    index_scan(rel, index, quals, colsel, true).unwrap_or((0.0, f64::INFINITY))
}

/// `cost_index`'s heap I/O lies between the uncorrelated case (each page
/// fetched at random, Mackert-Lohman) and the correlated one (the
/// selectivity's share of the pages, read in order), by the square of the
/// index's correlation.
fn index_scan(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    colsel: ColSel,
    full: bool,
) -> Option<(f64, f64)> {
    let (startup, index_total, sel) = index_cost(rel, index, quals, colsel, full)?;
    let tuples_fetched = clamp_row_est(sel * rel.tuples);
    let mut max_pages = index_pages_fetched(tuples_fetched, rel.pages);
    let mut min_pages = (sel * rel.pages).ceil();
    if index.visible > 0.0 {
        max_pages = (max_pages * (1.0 - index.visible)).ceil();
        min_pages = (min_pages * (1.0 - index.visible)).ceil();
    }
    let max_io = max_pages * RANDOM_PAGE_COST;
    let min_io = if min_pages > 0.0 {
        RANDOM_PAGE_COST + (min_pages - 1.0).max(0.0) * SEQ_PAGE_COST
    } else {
        0.0
    };
    let csquared = index.correlation * index.correlation;
    let io = max_io + csquared * (min_io - max_io);
    let others = || {
        quals
            .iter()
            .filter(|(f, q)| f != &index.field || !q.indexable())
            .map(|(_, q)| q)
    };
    let qs = qual_startup(others());
    Some((
        startup + qs,
        index_total + qs + io + (CPU_TUPLE_COST + qual_cost(others())) * tuples_fetched,
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
    cheapest_scan_path(rel, indexes, quals, ndistinct, rows).1
}

/// `cost_seqscan` through `quals`: its total; it starts at no cost, so it
/// is the cheapest scan to start.
pub fn seq_scan(rel: &Rel, quals: &[(String, Qual)]) -> f64 {
    seq_scan_path(rel, quals).1
}

/// `cost_seqscan`: `(startup, total)`, the quals' startup first.
fn seq_scan_path(rel: &Rel, quals: &[(String, Qual)]) -> (f64, f64) {
    let startup = qual_startup(quals.iter().map(|(_, q)| q));
    (
        startup,
        startup
            + SEQ_PAGE_COST * rel.pages
            + (CPU_TUPLE_COST + qual_cost(quals.iter().map(|(_, q)| q))) * rel.tuples,
    )
}

/// `cheapest_scan`'s path as `(startup, total)`: the cheapest in total, a
/// tie going to the cheaper startup (`set_cheapest`).
pub fn cheapest_scan_path(
    rel: &Rel,
    indexes: &[Index],
    quals: &[(String, Qual)],
    ndistinct: &dyn Fn(&str) -> f64,
    rows: f64,
) -> (f64, f64) {
    scan_path(
        rel,
        indexes,
        quals,
        &|f| ColSel::Ndistinct(ndistinct(f)),
        rows,
    )
    .0
}

/// `cheapest_scan_path` with each index's quals estimated by `colsel`, and
/// the index whose plain index scan it is, when it is one -- which returns
/// the rows in that index's order, where the others return the heap's.
pub fn scan_path<'a>(
    rel: &Rel,
    indexes: &'a [Index],
    quals: &[(String, Qual)],
    colsel: &dyn Fn(&str) -> ColSel,
    rows: f64,
) -> ((f64, f64), Option<&'a Index>) {
    let mut best = (seq_scan_path(rel, quals), None);
    for index in indexes {
        let cs = colsel(&index.field);
        for (cost, ordered) in [
            (bitmap_heap_cost(rel, index, quals, cs, rows), None),
            (index_scan_cost(rel, index, quals, cs), Some(index)),
        ] {
            let Some(cost) = cost else { continue };
            if cost.1 < best.0 .1 || (cost.1 == best.0 .1 && cost.0 < best.0 .0) {
                best = (cost, ordered);
            }
        }
    }
    best
}

/// How a table is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScanKind {
    Seq,
    /// A bitmap heap scan over the index at this position.
    Bitmap(usize),
    /// A plain index scan of the index at this position.
    Index(usize),
}

/// Every path `scan_path` weighs, with its `(startup, total)`: the
/// sequential scan, then each index's bitmap heap scan and index scan, in
/// that order -- so a caller can add `disable_cost` to the ones an
/// `enable_*` setting turns off before taking the cheapest.
pub fn scan_paths(
    rel: &Rel,
    indexes: &[Index],
    quals: &[(String, Qual)],
    colsel: &dyn Fn(&str) -> ColSel,
    rows: f64,
) -> Vec<(ScanKind, (f64, f64))> {
    let mut out = vec![(ScanKind::Seq, seq_scan_path(rel, quals))];
    for (i, index) in indexes.iter().enumerate() {
        let cs = colsel(&index.field);
        if let Some(c) = bitmap_heap_cost(rel, index, quals, cs, rows) {
            out.push((ScanKind::Bitmap(i), c));
        }
        if let Some(c) = index_scan_cost(rel, index, quals, cs) {
            out.push((ScanKind::Index(i), c));
        }
    }
    out
}

/// The scans of a table with statistics: the sequential scan, each index's
/// plain index scan, and the one bitmap heap scan `create_index_paths`
/// makes, over `choose_bitmap_and`'s pick of every bitmap path (`bitmap`).
/// The cheapest in total, a tie going to the cheaper startup, and the index
/// whose order a plain index scan returns.
pub fn measured_scan_path<'a>(
    rel: &Rel,
    indexes: &'a [Index],
    quals: &[(String, Qual)],
    colsel: &dyn Fn(&str) -> ColSel,
    bitmap: Option<&BitmapPath>,
) -> ((f64, f64), Option<&'a Index>) {
    let mut best = (seq_scan_path(rel, quals), None);
    let mut consider = |cost: (f64, f64), ordered: Option<&'a Index>| {
        if cost.1 < best.0 .1 || (cost.1 == best.0 .1 && cost.0 < best.0 .0) {
            best = (cost, ordered);
        }
    };
    if let Some(b) = bitmap {
        consider(bitmap_heap(rel, b, quals), None);
    }
    for index in indexes {
        if let Some(cost) = index_scan_cost(rel, index, quals, colsel(&index.field)) {
            consider(cost, Some(index));
        }
    }
    best
}

/// A bitmap path (`cost_bitmap_tree_node`'s view of it): its cost, its
/// selectivity, the clauses it reads (by position, for
/// `classify_index_clause_usage`), and whether it is a single index scan.
#[derive(Debug, Clone, PartialEq)]
pub struct BitmapPath {
    pub cost: f64,
    pub selectivity: f64,
    pub clauses: Vec<usize>,
    pub index: bool,
}

/// A bitmap index scan of `index` over `quals`' index quals on it: its
/// index cost plus a tenth of an operator for each of the `rows` the
/// relation yields (`cost_bitmap_tree_node`). `None` with no index qual.
pub fn bitmap_index_path(
    rel: &Rel,
    index: &Index,
    quals: &[(String, Qual)],
    colsel: ColSel,
    rows: f64,
    clauses: Vec<usize>,
) -> Option<BitmapPath> {
    let (_, total, selectivity) = index_cost(rel, index, quals, colsel, false)?;
    Some(BitmapPath {
        cost: total + 0.1 * CPU_OPERATOR_COST * rows,
        selectivity,
        clauses,
        index: true,
    })
}

/// `cost_bitmap_and_node`: the paths' costs, 100 operators for each after
/// the first, and their selectivities' product.
pub fn bitmap_and_path(paths: &[BitmapPath]) -> BitmapPath {
    let mut clauses: Vec<usize> = paths.iter().flat_map(|p| p.clauses.clone()).collect();
    clauses.sort_unstable();
    clauses.dedup();
    BitmapPath {
        cost: paths.iter().map(|p| p.cost).sum::<f64>()
            + 100.0 * CPU_OPERATOR_COST * paths.len().saturating_sub(1) as f64,
        selectivity: paths.iter().map(|p| p.selectivity).product(),
        clauses,
        index: false,
    }
}

/// `cost_bitmap_or_node` over each arm's path: their costs, 100 operators
/// for each arm after the first that is not a single index scan, and their
/// selectivities' sum, to at most 1. `clause` is the OR's position.
pub fn bitmap_or_path(arms: &[BitmapPath], clause: usize) -> BitmapPath {
    let mut cost = 0.0;
    for (i, arm) in arms.iter().enumerate() {
        cost += arm.cost;
        if i > 0 && !arm.index {
            cost += 100.0 * CPU_OPERATOR_COST;
        }
    }
    BitmapPath {
        cost,
        selectivity: f64::min(arms.iter().map(|a| a.selectivity).sum(), 1.0),
        clauses: vec![clause],
        index: false,
    }
}

/// `cost_bitmap_heap_scan` over `path`: `(startup, total)`.
pub fn bitmap_heap(rel: &Rel, path: &BitmapPath, quals: &[(String, Qual)]) -> (f64, f64) {
    bitmap_heap_from(rel, path.cost, path.selectivity, quals)
}

/// `choose_bitmap_and`: of paths reading the same clauses the cheaper (the
/// first on a tie); the rest in order of cost, then selectivity; from each
/// in turn, each later path that reads no clause already read is ANDed in
/// when that makes the bitmap heap scan cheaper; the cheapest result, the
/// first on a tie. `None` for no paths, or two distinct paths tied in both
/// cost and selectivity (`qsort` would order them unpredictably).
pub fn choose_bitmap_and(
    rel: &Rel,
    paths: &[BitmapPath],
    quals: &[(String, Qual)],
) -> Option<BitmapPath> {
    let mut kept: Vec<BitmapPath> = Vec::new();
    for p in paths {
        match kept.iter_mut().find(|k| k.clauses == p.clauses) {
            Some(k) if p.cost < k.cost => *k = p.clone(),
            Some(_) => {}
            None => kept.push(p.clone()),
        }
    }
    match kept.len() {
        0 => return None,
        1 => return kept.pop(),
        _ => {}
    }
    kept.sort_by(|a, b| {
        a.cost
            .total_cmp(&b.cost)
            .then(a.selectivity.total_cmp(&b.selectivity))
    });
    if kept
        .windows(2)
        .any(|w| w[0].cost == w[1].cost && w[0].selectivity == w[1].selectivity)
    {
        return None;
    }
    let estimate = |ps: &[BitmapPath]| {
        let path = if ps.len() == 1 {
            ps[0].clone()
        } else {
            bitmap_and_path(ps)
        };
        bitmap_heap(rel, &path, quals).1
    };
    let mut best: Option<(Vec<BitmapPath>, f64)> = None;
    for i in 0..kept.len() {
        let mut chosen = vec![kept[i].clone()];
        let mut cost = estimate(&chosen);
        for p in &kept[i + 1..] {
            if p.clauses
                .iter()
                .any(|c| chosen.iter().any(|q| q.clauses.contains(c)))
            {
                continue;
            }
            chosen.push(p.clone());
            let newcost = estimate(&chosen);
            if newcost < cost {
                cost = newcost;
            } else {
                chosen.pop();
            }
        }
        if best.as_ref().is_none_or(|b| cost < b.1) {
            best = Some((chosen, cost));
        }
    }
    let (chosen, _) = best?;
    Some(if chosen.len() == 1 {
        chosen.into_iter().next()?
    } else {
        bitmap_and_path(&chosen)
    })
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
    sorted_grouping_wins_over(input_total, rows, groups, ncols, aggs, ordered, &[])
}

/// `sorted_grouping_wins` with, besides the Sort + Group over the cheapest
/// scan, a Group over each scan already in the group keys' order
/// (`presorted`, `(startup, total, rows)`, as `limited_grouping_sorted`
/// takes them): the cheapest of the sorted paths in total, a tie within the
/// fuzz going to the cheaper startup, competes with the HashAggregate.
pub fn sorted_grouping_wins_over(
    input_total: f64,
    rows: f64,
    groups: f64,
    ncols: usize,
    aggs: Option<AggCosts>,
    ordered: bool,
    presorted: &[(f64, f64, f64)],
) -> bool {
    let (h, mut s) = grouping_costs(input_total, rows, groups, ncols, aggs);
    for &(startup, total, input_rows) in presorted {
        let g = presorted_group(startup, total, input_rows, groups, ncols, aggs);
        let f = STD_FUZZ_FACTOR;
        if g.1 * f < s.1 || (g.1 <= s.1 * f && s.1 <= g.1 * f && g.0 * f < s.0) {
            s = g;
        }
    }
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
        let g = presorted_group(startup, total, input_rows, groups, ncols, aggs);
        best_sorted = best_sorted.min(first(g));
    }
    best_sorted < hash
}

/// A Group (a GroupAggregate when `aggs` is `Some`) over an input already
/// in the keys' order: `(startup, total)` (`cost_group`, `cost_agg`).
fn presorted_group(
    startup: f64,
    total: f64,
    input_rows: f64,
    groups: f64,
    ncols: usize,
    aggs: Option<AggCosts>,
) -> (f64, f64) {
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
    (startup, total)
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
            correlation: 0.0,
            visible: 0.0,
            tuples: None,
            predicate: None,
            hash: false,
        }
    }
    fn q(filter: Document) -> Vec<(String, Qual)> {
        conjuncts(&filter).unwrap()
    }

    /// PostgreSQL 15.19, `partial-index`'s table without ANALYZE: 55 pages
    /// at 6,600 estimated tuples, and `test_idx on test_t (id) where val2 =
    /// 1` estimated at 255 tuples over 2 pages (`estimate_rel_size` from its
    /// widths). `where val2 = 1`: Bitmap Index Scan (0.00..4.31 rows=33),
    /// Bitmap Heap Scan (4.32..55.10); `where val2 = 1 and id = 10`: Index
    /// Scan (0.14..8.16 rows=1).
    #[test]
    fn a_partial_index_costs_as_postgres_does() {
        let rel = Rel {
            pages: 55.0,
            tuples: 6600.0,
        };
        let ix = Index {
            field: "id".into(),
            unique: false,
            columns: vec!["id".into()],
            pages: 2.0,
            tuples: Some(255.0),
            predicate: Some(0.005),
            ..pkey()
        };
        let nd = |_: &str| ColSel::Ndistinct(200.0);
        let by_val2 = q(bson::doc! {"val2": 1});
        // Its startup is the descent, which a Bitmap Index Scan's node
        // reports as 0.
        let (_, t, sel) = index_cost(&rel, &ix, &by_val2, nd("id"), false).unwrap();
        assert!(close(t, 4.31) && close(sel * rel.tuples, 33.0), "{t} {sel}");
        let (s, t) = bitmap_heap_cost(&rel, &ix, &by_val2, nd("id"), 33.0).unwrap();
        assert!(close(s, 4.32) && close(t, 55.10), "{s} {t}");
        let both = q(bson::doc! {"val2": 1, "id": 10});
        let (s, t) = index_scan_cost(&rel, &ix, &both, nd("id")).unwrap();
        assert!(close(s, 0.14) && close(t, 8.16), "{s} {t}");
        // A full index serves no scan without a qual of its own.
        let full = Index {
            predicate: None,
            tuples: None,
            ..ix
        };
        assert!(index_cost(&rel, &full, &by_val2, nd("id"), false).is_none());
    }

    #[test]
    fn index_and_bitmap_scans_cost_as_postgres_does() {
        let range = q(bson::doc! {"k": {"$gt": 5, "$lt": 30}});
        // Bitmap Index Scan (0.00..4.21 rows=6), Bitmap Heap Scan
        // (4.21..13.70 rows=6) over t3.
        let (_, it, _) =
            index_cost(&t3(), &pkey(), &range, ColSel::Ndistinct(1200.0), false).unwrap();
        assert!(close(it, 4.21), "{it}");
        let (s, t) =
            bitmap_heap_cost(&t3(), &pkey(), &range, ColSel::Ndistinct(1200.0), 6.0).unwrap();
        assert!(close(s, 4.21) && close(t, 13.70), "{s} {t}");
        // t2: Bitmap Heap Scan (4.27..14.97 rows=11).
        let (s, t) =
            bitmap_heap_cost(&t2(), &pkey(), &range, ColSel::Ndistinct(2260.0), 11.0).unwrap();
        assert!(close(s, 4.27) && close(t, 14.97), "{s} {t}");
        // `k > 5`: Bitmap Index Scan 9.80, Heap (9.99..29.40 rows=753).
        let gt = q(bson::doc! {"k": {"$gt": 5}});
        let (s, t) =
            bitmap_heap_cost(&t2(), &pkey(), &gt, ColSel::Ndistinct(2260.0), 753.0).unwrap();
        assert!(close(s, 9.99) && close(t, 29.40), "{s} {t}");
        // `k = 3`: Index Scan (0.15..8.17 rows=1).
        let eq = q(bson::doc! {"k": 3});
        let (s, t) = index_scan_cost(&t2(), &pkey(), &eq, ColSel::Ndistinct(2260.0)).unwrap();
        assert!(close(s, 0.15) && close(t, 8.17), "{s} {t}");
        // `k in (3, 5, 7)`: Bitmap Index Scan 8.49, Heap (8.49..15.60).
        let saop = q(bson::doc! {"k": {"$in": [3, 5, 7]}});
        let (s, t) =
            bitmap_heap_cost(&t2(), &pkey(), &saop, ColSel::Ndistinct(2260.0), 3.0).unwrap();
        assert!(close(s, 8.49) && close(t, 15.60), "{s} {t}");
        // A `<>` beside them is rechecked on the heap: (4.27..14.99).
        let ne = q(bson::doc! {"k": {"$gt": 5, "$lt": 30}, "g": {"$ne": 2}});
        let (_, t) =
            bitmap_heap_cost(&t2(), &pkey(), &ne, ColSel::Ndistinct(2260.0), 11.0).unwrap();
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

    /// PostgreSQL 15.19 over 300 rows (2 pages) of (k, g, t): a sequential
    /// scan filtering `g in (0, .., 8)`, a hashed list, costs 0.02..6.52 --
    /// a startup of one hash a value, then a hash and a comparison a row.
    #[test]
    fn a_hashed_in_list_costs_as_postgres_does() {
        let rel = Rel {
            pages: 2.0,
            tuples: 300.0,
        };
        let (s, t) = seq_scan_path(&rel, &[("g".into(), Qual::In(9))]);
        assert!(close(s, 0.02) && close(t, 6.52), "{s} {t}");
    }

    /// PostgreSQL 15.19, 10,000 analysed rows of (k primary key, g, t) with
    /// an index on g: `(g = 1 and k < 100) or k > 9000` is a BitmapOr
    /// (40.92) of a BitmapAnd (20.88) of the key's scan (5.03, 0.0099) and
    /// g's (15.10, 0.0909), and the key's scan for `k > 9000` (19.79,
    /// 0.1), each charged a tenth of an operator for 1,008 rows.
    #[test]
    fn bitmap_and_and_or_cost_as_postgres_does() {
        let rel = Rel {
            pages: 55.0,
            tuples: 10000.0,
        };
        let path = |cost: f64, selectivity: f64, clause: usize| BitmapPath {
            cost: cost + 0.1 * CPU_OPERATOR_COST * 1008.0,
            selectivity,
            clauses: vec![clause],
            index: true,
        };
        let arm =
            choose_bitmap_and(&rel, &[path(15.10, 0.0909, 0), path(5.03, 0.0099, 1)], &[]).unwrap();
        assert!(!arm.index && close(arm.cost, 20.88), "{arm:?}");
        let or = bitmap_or_path(&[arm, path(19.79, 0.1, 2)], 0);
        // Three costs PostgreSQL displays to two places, summed.
        assert!((or.cost - 40.92).abs() < 0.01, "{or:?}");
    }
}
