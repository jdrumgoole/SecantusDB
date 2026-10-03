//! A correlated subquery whose outer references are all EQUALITIES, run once.
//!
//! `correlated.rs` runs a correlated subquery per distinct outer value: plan,
//! scan, repeat. When every `$N` in the inner WHERE stands in a top-level
//! conjunct `inner_col = $N`, the per-row query is the same scan filtered on
//! those columns -- so it is run ONCE, without those conjuncts and with the
//! columns appended to its select list, and the rows are indexed by them.
//! Each outer row is then a hash lookup. (2,000 x 2,000 rows, two
//! equalities, debug build: 7.3 s per-row; PostgreSQL plans it as a hash
//! semi-join.)
//!
//! Only shapes whose rows are a filter of the scan qualify -- optionally
//! ordered (ORDER BY over the inner side, with LIMIT / OFFSET applied per
//! key) or one groupable aggregate: no grouping, DISTINCT, window, set
//! operation, CTE, locking clause, nested subquery or function in the select
//! list (a volatile one could not be run once). And only key values whose
//! hash equality IS SQL equality are hashed: integers, floats folded to their
//! numeric value, numerics (by their exact value, never against a float),
//! text, booleans, dates and timestamps. Anything else -- a collation, a
//! mixed-type comparison, NULL -- falls back to the per-row path, which keeps
//! every answer it gave before. The FROM clause may be tables joined with
//! JOIN as well as with commas.

use super::*;
use std::collections::HashMap;

/// A key value under SQL equality: integral numbers fold together across
/// int / bigint / float, so `1 = 1.0` hashes alike.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Key {
    Int(i64),
    Float(u64),
    /// A non-integral `numeric`, as its exact value: (negative, the digits
    /// without leading or trailing zeros, the power of ten of the last one).
    Dec(bool, String, i64),
    NaN,
    Text(String),
    Bool(bool),
    Time(i64),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Number,
    Text,
    Bool,
    Time,
}

impl Key {
    fn family(&self) -> Family {
        match self {
            Key::Int(_) | Key::Float(_) | Key::Dec(..) | Key::NaN => Family::Number,
            Key::Text(_) => Family::Text,
            Key::Bool(_) => Family::Bool,
            Key::Time(_) => Family::Time,
        }
    }
}

fn key_of(v: &Bson) -> Option<Key> {
    match v {
        Bson::Int32(i) => Some(Key::Int(i64::from(*i))),
        Bson::Int64(i) => Some(Key::Int(*i)),
        Bson::Double(d) => {
            if d.is_nan() {
                Some(Key::NaN)
            } else if d.fract() == 0.0 && d.abs() < 9.0e15 {
                #[allow(clippy::cast_possible_truncation)]
                Some(Key::Int(*d as i64))
            } else {
                Some(Key::Float(d.to_bits()))
            }
        }
        Bson::String(s) => Some(Key::Text(s.clone())),
        Bson::Boolean(b) => Some(Key::Bool(*b)),
        Bson::DateTime(d) => Some(Key::Time(d.timestamp_millis())),
        Bson::Decimal128(d) => decimal_key(&d.to_string()),
        _ => None,
    }
}

/// A `numeric`'s key: its exact value, folded to `Key::Int` when integral
/// (so `5::numeric` meets `5`). `None` for an infinity or what does not parse.
fn decimal_key(text: &str) -> Option<Key> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("nan") {
        return Some(Key::NaN);
    }
    let (neg, t) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (mantissa, exp) = match t.find(['e', 'E']) {
        Some(i) => (&t[..i], t[i + 1..].parse::<i64>().ok()?),
        None => (t, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if int_part.is_empty() && frac_part.is_empty()
        || !int_part
            .bytes()
            .chain(frac_part.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut digits: String = format!("{int_part}{frac_part}");
    let mut exp = exp - i64::try_from(frac_part.len()).ok()?;
    let lead = digits.len() - digits.trim_start_matches('0').len();
    digits.drain(..lead);
    if digits.is_empty() {
        return Some(Key::Int(0));
    }
    while digits.ends_with('0') {
        digits.pop();
        exp += 1;
    }
    if exp >= 0 && i64::try_from(digits.len()).ok()? + exp <= 18 {
        let mut v: i64 = digits.parse().ok()?;
        for _ in 0..exp {
            v *= 10;
        }
        return Some(Key::Int(if neg { -v } else { v }));
    }
    Some(Key::Dec(neg, digits, exp))
}

/// Which inexact number kinds a key column holds: a float and a `numeric`
/// that are equal in SQL (`0.1::float8 = 0.1::numeric`) hash apart, so a
/// lookup that would compare one with the other goes the per-row way.
#[derive(Clone, Copy, Default)]
struct Kinds {
    float: bool,
    dec: bool,
}

impl Kinds {
    fn note(&mut self, k: &Key) {
        match k {
            Key::Float(_) => self.float = true,
            Key::Dec(..) => self.dec = true,
            _ => {}
        }
    }
    /// Could `k` miss a row it equals in SQL?
    fn clashes(self, k: &Key) -> bool {
        match k {
            Key::Float(_) => self.dec,
            Key::Dec(..) => self.float,
            _ => false,
        }
    }
}

/// One subquery's index: the projected rows grouped by their key columns.
/// A key's rows, each as (projection, the filter columns' values).
type Groups = HashMap<Vec<Key>, Vec<(Vec<Bson>, Vec<Bson>)>>;

struct Index {
    /// For each key column, which `$N` (0-based) it is compared with.
    params: Vec<usize>,
    /// An aggregate subquery: a key with no group is not "no rows" but the
    /// aggregate of nothing (`count` 0, `sum` NULL), which the per-row path
    /// answers.
    aggregate: bool,
    /// An aggregate under comparison filters, computed per outer row.
    late: Option<Late>,
    /// `LIMIT` / `OFFSET`, applied within each key's rows (scan order, as
    /// the per-row query would see them).
    limit: Option<usize>,
    offset: usize,
    families: Vec<Option<Family>>,
    /// Per key column, the inexact number kinds it holds.
    kinds: Vec<Kinds>,
    /// Non-equality conjuncts `col op $N`, applied to a key's rows per
    /// outer row (numbers and timestamps only; text order is a collation's).
    filters: Vec<Filter>,
    /// Each row as (projection, the filter columns' values).
    rows: Groups,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Ne,
}

impl Op {
    fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "<" => Op::Lt,
            "<=" => Op::Le,
            ">" => Op::Gt,
            ">=" => Op::Ge,
            "<>" | "!=" => Op::Ne,
            _ => return None,
        })
    }
    /// `col op $N` read the other way round, for `$N op col`.
    fn flipped(self) -> Op {
        match self {
            Op::Lt => Op::Gt,
            Op::Le => Op::Ge,
            Op::Gt => Op::Lt,
            Op::Ge => Op::Le,
            Op::Ne => Op::Ne,
        }
    }
    fn holds(self, o: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match self {
            Op::Lt => o == Less,
            Op::Le => o != Greater,
            Op::Gt => o == Greater,
            Op::Ge => o != Less,
            Op::Ne => o != Equal,
        }
    }
}

/// An aggregate computed per outer row over a key's FILTERED rows: the
/// inner query runs un-aggregated, projecting the aggregate's argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Late {
    /// `count(*)`.
    CountStar,
    /// `count(col)`: the non-NULL values.
    Count,
    Min,
    Max,
    /// `sum` / `avg` of an `integer` / `smallint` column: a `bigint` sum,
    /// and that sum divided by the count as `numeric` (PostgreSQL's
    /// `int8_avg` is exactly `numeric_div` of the two).
    Sum,
    Avg,
}

impl Late {
    /// The aggregate of `values` (one per filtered row), or `None` when two
    /// of them cannot be ordered here.
    fn over(self, values: &[&Bson]) -> Option<Bson> {
        let present = values.iter().filter(|v| !matches!(v, Bson::Null));
        match self {
            Late::CountStar => Some(Bson::Int64(i64::try_from(values.len()).ok()?)),
            Late::Count => Some(Bson::Int64(i64::try_from(present.count()).ok()?)),
            Late::Min | Late::Max => {
                let want = if self == Late::Min {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
                let mut best: Option<&Bson> = None;
                for v in present {
                    best = Some(match best {
                        None => v,
                        Some(b) if sql_order(v, b)? == want => v,
                        Some(b) => b,
                    });
                }
                Some(best.cloned().unwrap_or(Bson::Null))
            }
            Late::Sum | Late::Avg => {
                let mut sum: i64 = 0;
                let mut n: i64 = 0;
                for v in present {
                    let Bson::Int32(i) = v else {
                        return None;
                    };
                    sum = sum.checked_add(i64::from(*i))?;
                    n += 1;
                }
                if n == 0 {
                    return Some(Bson::Null);
                }
                if self == Late::Sum {
                    return Some(Bson::Int64(sum));
                }
                thread_local! {
                    static DIV: Option<pg_query::protobuf::Node> =
                        crate::domains::parse_default_sql("$1::numeric / $2::numeric").ok();
                }
                DIV.with(|d| {
                    crate::const_value(d.as_ref()?, &[Bson::Int64(sum), Bson::Int64(n)]).ok()
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Filter {
    param: usize,
    op: Op,
}

/// SQL's ordering of two numbers, two timestamps or two byte-ordered texts,
/// or `None` for anything else (numeric, mixed families, NULL) -- which the
/// per-row path then answers. NaN sorts above every number and equals NaN, as in PostgreSQL.
fn sql_order(a: &Bson, b: &Bson) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let num = |v: &Bson| -> Option<(Option<i64>, f64)> {
        match v {
            Bson::Int32(i) => Some((Some(i64::from(*i)), f64::from(*i))),
            #[allow(clippy::cast_precision_loss)]
            Bson::Int64(i) => Some((Some(*i), *i as f64)),
            Bson::Double(d) => Some((None, *d)),
            _ => None,
        }
    };
    match (a, b) {
        (Bson::DateTime(x), Bson::DateTime(y)) => {
            Some(x.timestamp_millis().cmp(&y.timestamp_millis()))
        }
        // Only reached for a filter column `build` found to be plain text
        // under byte order (`text_order_allowed`).
        (Bson::String(x), Bson::String(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
        _ => {
            let ((ai, af), (bi, bf)) = (num(a)?, num(b)?);
            if let (Some(x), Some(y)) = (ai, bi) {
                return Some(x.cmp(&y));
            }
            Some(match (af.is_nan(), bf.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => af.partial_cmp(&bf)?,
            })
        }
    }
}

enum Entry {
    /// The shape does not qualify, or a value could not be hashed.
    No,
    Built(Index),
}

thread_local! {
    static CACHE: std::cell::RefCell<Option<HashMap<String, Entry>>> =
        const { std::cell::RefCell::new(None) };
    /// Set while a correlated subquery of the running statement is being
    /// run (`correlated::run_direct`), and cleared for a user-code body.
    static IN_SUBQUERY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` as a subquery of the running statement: a scope it enters shares
/// the statement's cache.
pub(crate) fn as_subquery<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            IN_SUBQUERY.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(IN_SUBQUERY.with(|c| c.replace(true)));
    f()
}

/// Enables the cache for one statement's execution and drops it after.
///
/// A scope entered while one is already active -- a subquery of the same
/// statement run through a nested runner (`as_subquery`) -- SHARES the statement's cache: it
/// reads the same data, and rebuilding an inner level's index for every
/// outer row is what made a nested correlated `EXISTS` quadratic (2,000 x
/// 2,000 rows: 36 s). User code (a function, trigger or procedure body) runs
/// its statements under [`Scope::fresh`] instead, since they may read the
/// body's own earlier writes.
pub(crate) struct Scope(Option<Option<HashMap<String, Entry>>>);

impl Scope {
    pub(crate) fn enter() -> Self {
        if IN_SUBQUERY.with(std::cell::Cell::get) && CACHE.with(|c| c.borrow().is_some()) {
            return Scope(None);
        }
        Self::fresh()
    }

    /// A cache of its own, whatever is active, restored on drop.
    pub(crate) fn fresh() -> Self {
        Scope(Some(CACHE.with(|c| c.replace(Some(HashMap::new())))))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(prev) = self.0.take() {
            CACHE.with(|c| *c.borrow_mut() = prev);
        }
    }
}

/// Run `f` -- a user function's, trigger's or procedure's body -- with a
/// semi-join cache of its own, so its statements never read an index built
/// before the body's own writes.
pub fn with_fresh_subquery_cache<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            IN_SUBQUERY.with(|c| c.set(self.0));
        }
    }
    let _scope = Scope::fresh();
    // Each statement of the body is a statement of its own.
    let _restore = Restore(IN_SUBQUERY.with(|c| c.replace(false)));
    f()
}

type Runner = fn(&str, &[Bson]) -> Result<Vec<Vec<Bson>>>;

/// The rows `sql` returns for `params`, from the index -- or `None` when the
/// shape or these values need the ordinary per-row path.
pub(crate) fn lookup(sql: &str, params: &[Bson], run: Runner) -> Result<Option<Vec<Vec<Bson>>>> {
    let active = CACHE.with(|c| c.borrow().is_some());
    if !active {
        return Ok(None);
    }
    let built = CACHE.with(|c| c.borrow().as_ref().is_some_and(|m| m.contains_key(sql)));
    if !built {
        let entry = build(sql, run)?;
        CACHE.with(|c| {
            if let Some(m) = c.borrow_mut().as_mut() {
                m.insert(sql.to_string(), entry);
            }
        });
    }
    CACHE.with(|c| {
        let cache = c.borrow();
        let Some(Entry::Built(ix)) = cache.as_ref().and_then(|m| m.get(sql)) else {
            return Ok(None);
        };
        let mut key = Vec::with_capacity(ix.params.len());
        for (i, &p) in ix.params.iter().enumerate() {
            let Some(v) = params.get(p) else {
                return Ok(None);
            };
            // `x = NULL` matches nothing; the per-row path says so itself.
            let Some(k) = key_of(v) else {
                return Ok(None);
            };
            // A comparison across families (text against a number) is the
            // per-row path's error or coercion, never a quiet miss here.
            if ix.families[i].is_some_and(|f| f != k.family()) || ix.kinds[i].clashes(&k) {
                return Ok(None);
            }
            key.push(k);
        }
        let mut bounds = Vec::with_capacity(ix.filters.len());
        for f in &ix.filters {
            let Some(v) = params.get(f.param) else {
                return Ok(None);
            };
            if matches!(v, Bson::Null) {
                // `col < NULL` is never true: no row qualifies.
                return Ok(match ix.late {
                    Some(late) => late.over(&[]).map(|v| vec![vec![v]]),
                    None => Some(Vec::new()),
                });
            }
            bounds.push(v);
        }
        match ix.rows.get(&key) {
            Some(rows) => {
                let mut out = Vec::new();
                for (proj, fvals) in rows {
                    let mut pass = true;
                    for ((f, bound), v) in ix.filters.iter().zip(&bounds).zip(fvals) {
                        if matches!(v, Bson::Null) {
                            pass = false;
                            break;
                        }
                        let Some(o) = sql_order(v, bound) else {
                            return Ok(None); // not comparable here: per-row path
                        };
                        if !f.op.holds(o) {
                            pass = false;
                            break;
                        }
                    }
                    if pass {
                        out.push(proj.clone());
                    }
                }
                if let Some(late) = ix.late {
                    let values: Vec<&Bson> = out.iter().filter_map(|r| r.first()).collect();
                    return Ok(late.over(&values).map(|v| vec![vec![v]]));
                }
                Ok(Some(
                    out.into_iter()
                        .skip(ix.offset)
                        .take(ix.limit.unwrap_or(usize::MAX))
                        .collect(),
                ))
            }
            None => match ix.late {
                Some(late) => Ok(late.over(&[]).map(|v| vec![vec![v]])),
                None if ix.aggregate => Ok(None),
                None => Ok(Some(Vec::new())),
            },
        }
    })
}

fn build(sql: &str, run: Runner) -> Result<Entry> {
    let Some(Rewritten {
        sql: rewritten,
        params,
        filters,
        aggregate,
        late,
        limit,
        offset,
    }) = rewrite(sql)
    else {
        return Ok(Entry::No);
    };
    let rows = run(&rewritten, &[])?;
    let nkeys = params.len();
    let mut families: Vec<Option<Family>> = vec![None; nkeys];
    let mut kinds: Vec<Kinds> = vec![Kinds::default(); nkeys];
    let nfilters = filters.len();
    let text_ok = crate::collation::text_is_byte_ordered();
    let mut index: Groups = HashMap::new();
    for mut row in rows {
        if row.len() < nkeys + 2 * nfilters + usize::from(late.is_some()) {
            return Ok(Entry::No);
        }
        // A late min / max orders its argument here, so the argument must be
        // orderable as a filter column is (its type is the last column).
        if let Some(late) = late {
            let ty = row.pop().unwrap_or(Bson::Null);
            if matches!(late, Late::Min | Late::Max)
                && !row.first().is_some_and(|v| orderable(v, &ty, text_ok))
            {
                return Ok(Entry::No);
            }
            // A sum / avg only over 32-bit-or-narrower integers, whose sum
            // is a `bigint` computed exactly here.
            if matches!(late, Late::Sum | Late::Avg)
                && (!matches!(&ty, Bson::String(t) if t == "integer" || t == "smallint")
                    || !row
                        .first()
                        .is_some_and(|v| matches!(v, Bson::Int32(_) | Bson::Null)))
            {
                return Ok(Entry::No);
            }
        }
        // Each filter column, then its type's name (`pg_typeof(col)::text`).
        let ftypes = row.split_off(row.len() - nfilters);
        let fvals = row.split_off(row.len() - nfilters);
        if !fvals
            .iter()
            .zip(&ftypes)
            .all(|(v, t)| orderable(v, t, text_ok))
        {
            return Ok(Entry::No);
        }
        let keys = row.split_off(row.len() - nkeys);
        let mut key = Vec::with_capacity(nkeys);
        let mut null = false;
        for (i, v) in keys.iter().enumerate() {
            if matches!(v, Bson::Null) {
                null = true; // a NULL key equals no outer value
                continue;
            }
            let Some(k) = key_of(v) else {
                return Ok(Entry::No);
            };
            // A nondeterministic collation (only `CREATE COLLATION ...
            // deterministic = false` makes one) calls unequal bytes equal,
            // which a hash cannot see.
            if k.family() == Family::Text && nondeterministic_collation_exists() {
                return Ok(Entry::No);
            }
            match families[i] {
                None => families[i] = Some(k.family()),
                Some(f) if f != k.family() => return Ok(Entry::No),
                Some(_) => {}
            }
            kinds[i].note(&k);
            key.push(k);
        }
        if !null {
            index.entry(key).or_default().push((row, fvals));
        }
    }
    Ok(Entry::Built(Index {
        params,
        aggregate,
        late,
        limit,
        offset,
        families,
        kinds,
        filters,
        rows: index,
    }))
}

/// Can `v` (of the type named `ty`) be ordered by `sql_order`? Numbers and
/// timestamps; text only when it is plain text ordered by its bytes. A
/// numeric and anything else go the per-row way.
fn orderable(v: &Bson, ty: &Bson, text_ok: bool) -> bool {
    match v {
        Bson::Null | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::DateTime(_) => true,
        Bson::String(_) => {
            text_ok
                && matches!(ty, Bson::String(t)
                    if matches!(t.as_str(), "text" | "character varying" | "name"))
        }
        _ => false,
    }
}

fn nondeterministic_collation_exists() -> bool {
    crate::extension_installed("citext")
        || crate::collation::user_collations()
            .iter()
            .any(|c| !c.deterministic)
}

struct Rewritten {
    sql: String,
    params: Vec<usize>,
    filters: Vec<Filter>,
    aggregate: bool,
    late: Option<Late>,
    limit: Option<usize>,
    offset: usize,
}

/// Aggregates whose value over a key's rows is the per-row query's answer
/// for that key once the query is grouped by it.
const GROUPABLE: &[&str] = &[
    "count", "sum", "avg", "min", "max", "bool_and", "bool_or", "every",
];

fn is_groupable_aggregate(n: &N) -> bool {
    match n {
        N::FuncCall(f) => {
            f.over.is_none()
                && f.agg_filter.is_none()
                && f.agg_order.is_empty()
                && !f.agg_within_group
                && matches!(f.funcname.last().and_then(|p| p.node.as_ref()),
                    Some(N::String(s)) if GROUPABLE.contains(&s.sval.as_str()))
        }
        _ => false,
    }
}

fn const_count(n: Option<&pg_query::protobuf::Node>) -> Option<Option<usize>> {
    match n.and_then(|n| n.node.as_ref()) {
        None => Some(None),
        Some(N::AConst(c)) => match &c.val {
            Some(pg_query::protobuf::a_const::Val::Ival(i)) => {
                usize::try_from(i.ival).ok().map(Some)
            }
            _ if c.isnull => Some(None),
            _ => None,
        },
        _ => None,
    }
}

/// `sql` with each `col = $N` conjunct removed and `col` appended to the
/// select list (and grouped by, for an aggregate), plus which `$N` each
/// appended column answers -- or `None` when the shape does not qualify.
fn rewrite(sql: &str) -> Option<Rewritten> {
    let mut parsed = pg_query::parse(sql).ok()?.protobuf;
    if parsed.stmts.len() != 1 {
        return None;
    }
    let stmt = parsed.stmts[0].stmt.as_mut()?;
    let Some(N::SelectStmt(s)) = stmt.node.as_mut() else {
        return None;
    };
    if s.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || !s.group_clause.is_empty()
        || s.having_clause.is_some()
        || !s.distinct_clause.is_empty()
        || s.with_clause.is_some()
        || !s.window_clause.is_empty()
        || !s.locking_clause.is_empty()
        || !s.values_lists.is_empty()
        || s.into_clause.is_some()
        || s.from_clause.is_empty()
    {
        return None;
    }
    // The select list and FROM must not reach the outer row or do anything
    // that running once would change: no parameter, function or subquery.
    let limit = const_count(s.limit_count.as_deref())?;
    let offset = const_count(s.limit_offset.as_deref())?.unwrap_or(0);
    let aggregate = s
        .target_list
        .iter()
        .any(|t| contains(t, &is_groupable_aggregate));
    if aggregate && (limit.is_some() || offset > 0) {
        return None;
    }
    // An ORDER BY is kept in the run-once query: each key's rows are then a
    // subsequence of one ordered list, so they come out in the order the
    // per-row query gives them (and LIMIT / OFFSET apply per key). Not with
    // an aggregate, and only over the inner side.
    if !s.sort_clause.is_empty()
        && (aggregate
            || s.sort_clause
                .iter()
                .any(|n| contains(n, &|n| matches!(n, N::ParamRef(_) | N::SubLink(_)))))
    {
        return None;
    }
    s.limit_count = None;
    s.limit_offset = None;
    for t in &s.target_list {
        let other_call = contains(t, &|n| {
            matches!(n, N::FuncCall(_)) && !is_groupable_aggregate(n)
        });
        if other_call || contains(t, &|n| matches!(n, N::ParamRef(_) | N::SubLink(_))) {
            return None;
        }
    }
    for f in &s.from_clause {
        if contains(f, &|n| {
            matches!(n, N::ParamRef(_) | N::FuncCall(_) | N::SubLink(_))
        }) || !tables_only(f)
        {
            return None;
        }
    }
    let where_clause = s.where_clause.take()?;
    let mut conjuncts = Vec::new();
    split_and(*where_clause, &mut conjuncts);
    let mut keep = Vec::new();
    let mut keys: Vec<(pg_query::protobuf::Node, usize)> = Vec::new();
    let mut filters: Vec<(pg_query::protobuf::Node, Filter)> = Vec::new();
    for c in conjuncts {
        if let Some((col, p, op)) = column_op_param(&c) {
            match op {
                None => keys.push((col, p)),
                Some(op) => filters.push((col, Filter { param: p, op })),
            }
            continue;
        }
        // Every other conjunct must read only the inner side.
        if contains(&c, &|n| matches!(n, N::ParamRef(_) | N::SubLink(_))) {
            return None;
        }
        keep.push(c);
    }
    if keys.is_empty() && filters.is_empty() {
        return None;
    }
    // A filter changes which rows an aggregate sees per outer row, which a
    // grouping by the keys alone cannot express: `count` / `min` / `max`
    // of one column are then computed per outer row (`Late`), the rows
    // projecting the argument; any other aggregate goes the per-row way.
    let mut late = None;
    let mut late_arg = None;
    if aggregate && !filters.is_empty() {
        let (kind, arg) = late_aggregate(&s.target_list)?;
        late = Some(kind);
        late_arg = Some(arg.clone());
        s.target_list = vec![target(arg)];
    }
    let aggregate = aggregate && late.is_none();
    s.where_clause = and_of(keep).map(Box::new);
    let mut params = Vec::new();
    for (col, p) in keys {
        if aggregate {
            s.group_clause.push(col.clone());
        }
        s.target_list.push(pg_query::protobuf::Node {
            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                val: Some(Box::new(col)),
                location: -1,
                ..Default::default()
            }))),
        });
        params.push(p);
    }
    let mut filter_list = Vec::new();
    let mut types = Vec::new();
    for (col, f) in filters {
        types.push(type_name_of(col.clone())?);
        s.target_list.push(target(col));
        filter_list.push(f);
    }
    s.target_list.extend(types.into_iter().map(target));
    if let Some(arg) = late_arg {
        s.target_list.push(target(type_name_of(arg)?));
    }
    let text = pg_query::deparse(&parsed).ok()?;
    Some(Rewritten {
        sql: text,
        params,
        filters: filter_list,
        aggregate,
        late,
        limit,
        offset,
    })
}

/// `col op $N` or `$N op col`, as (col, N - 1, op) -- `None` for the op
/// meaning equality (a key), `Some` for a filter.
#[allow(clippy::type_complexity)]
fn column_op_param(
    n: &pg_query::protobuf::Node,
) -> Option<(pg_query::protobuf::Node, usize, Option<Op>)> {
    let Some(N::AExpr(e)) = n.node.as_ref() else {
        return None;
    };
    if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 {
        return None;
    }
    let name = match e.name.as_slice() {
        [op] => match op.node.as_ref() {
            Some(N::String(s)) => s.sval.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let op = if name == "=" {
        None
    } else {
        Some(Op::parse(&name)?)
    };
    let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
    let param = |x: &pg_query::protobuf::Node| match x.node.as_ref() {
        Some(N::ParamRef(p)) if p.number >= 1 => usize::try_from(p.number - 1).ok(),
        _ => None,
    };
    let column = |x: &pg_query::protobuf::Node| matches!(x.node.as_ref(), Some(N::ColumnRef(_)));
    match (column(l), param(r), param(l), column(r)) {
        (true, Some(p), _, _) => Some((l.clone(), p, op)),
        (_, _, Some(p), true) => Some((r.clone(), p, op.map(Op::flipped))),
        _ => None,
    }
}

/// The select list's ONE aggregate, when it is `count(*)`, or `count` / `min`
/// / `max` of a column with no DISTINCT, FILTER or ORDER BY: its kind and
/// the value to project in its place.
fn late_aggregate(
    targets: &[pg_query::protobuf::Node],
) -> Option<(Late, pg_query::protobuf::Node)> {
    let [t] = targets else {
        return None;
    };
    let Some(N::ResTarget(rt)) = t.node.as_ref() else {
        return None;
    };
    let Some(N::FuncCall(f)) = rt.val.as_deref()?.node.as_ref() else {
        return None;
    };
    if !is_groupable_aggregate(&N::FuncCall(f.clone())) || f.agg_distinct || f.funcname.len() != 1 {
        return None;
    }
    let name = match f.funcname[0].node.as_ref() {
        Some(N::String(s)) => s.sval.as_str(),
        _ => return None,
    };
    if f.agg_star {
        return (name == "count").then(|| {
            let one = pg_query::parse("SELECT 1")
                .ok()?
                .protobuf
                .stmts
                .pop()?
                .stmt?;
            let N::SelectStmt(mut sel) = one.node? else {
                return None;
            };
            let N::ResTarget(rt) = sel.target_list.pop()?.node? else {
                return None;
            };
            Some((Late::CountStar, *rt.val?))
        })?;
    }
    let [arg] = f.args.as_slice() else {
        return None;
    };
    if !matches!(arg.node.as_ref(), Some(N::ColumnRef(_))) {
        return None;
    }
    let kind = match name {
        "count" => Late::Count,
        "min" => Late::Min,
        "max" => Late::Max,
        "sum" => Late::Sum,
        "avg" => Late::Avg,
        _ => return None,
    };
    Some((kind, arg.clone()))
}

/// A select-list entry for `val`.
fn target(val: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            val: Some(Box::new(val)),
            location: -1,
            ..Default::default()
        }))),
    }
}

/// `pg_typeof(col)::text`: a filter column's type, which decides whether
/// its values may be ordered here.
fn type_name_of(col: pg_query::protobuf::Node) -> Option<pg_query::protobuf::Node> {
    let mut parsed = pg_query::parse("SELECT pg_typeof(x)::text").ok()?.protobuf;
    let stmt = parsed.stmts.pop()?.stmt?;
    let N::SelectStmt(mut sel) = stmt.node? else {
        return None;
    };
    let N::ResTarget(rt) = sel.target_list.pop()?.node? else {
        return None;
    };
    let mut cast = rt.val?;
    let Some(N::TypeCast(tc)) = cast.node.as_mut() else {
        return None;
    };
    let Some(N::FuncCall(fc)) = tc.arg.as_mut()?.node.as_mut() else {
        return None;
    };
    fc.args = vec![col];
    Some(*cast)
}

/// Is a FROM item stored tables only -- one, or several joined?
fn tables_only(n: &pg_query::protobuf::Node) -> bool {
    match n.node.as_ref() {
        Some(N::RangeVar(_)) => true,
        Some(N::JoinExpr(j)) => {
            j.larg.as_deref().is_some_and(tables_only) && j.rarg.as_deref().is_some_and(tables_only)
        }
        _ => false,
    }
}

fn contains(n: &pg_query::protobuf::Node, f: &dyn Fn(&N) -> bool) -> bool {
    let mut n = n.clone();
    let mut found = false;
    let _ = walk_expr(&mut n, &mut |x| {
        if let Some(inner) = x.node.as_ref() {
            if f(inner) {
                found = true;
            }
        }
        Ok(())
    });
    found
}

fn split_and(n: pg_query::protobuf::Node, out: &mut Vec<pg_query::protobuf::Node>) {
    match n.node {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in b.args {
                split_and(a, out);
            }
        }
        _ => out.push(n),
    }
}

fn and_of(mut items: Vec<pg_query::protobuf::Node>) -> Option<pg_query::protobuf::Node> {
    match items.len() {
        0 => None,
        1 => items.pop(),
        _ => Some(pg_query::protobuf::Node {
            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                boolop: pg_query::protobuf::BoolExprType::AndExpr as i32,
                args: items,
                location: -1,
                ..Default::default()
            }))),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equalities_become_appended_key_columns() {
        let r = rewrite("SELECT 1 FROM t WHERE t.x = $1 AND $2 = t.y AND t.f").expect("qualifies");
        assert_eq!(r.sql, "SELECT 1, t.x, t.y FROM t WHERE t.f");
        assert_eq!(r.params, vec![0, 1]);
        assert!(!r.aggregate);
    }

    #[test]
    fn an_aggregate_is_grouped_by_its_keys() {
        let r = rewrite("SELECT count(*) FROM t WHERE t.x = $1").expect("qualifies");
        assert_eq!(r.sql, "SELECT count(*), t.x FROM t GROUP BY t.x");
        assert!(r.aggregate);
    }

    #[test]
    fn limit_and_offset_are_applied_per_key() {
        let r = rewrite("SELECT t.v FROM t WHERE t.x = $1 LIMIT 1 OFFSET 2").expect("qualifies");
        assert_eq!(r.sql, "SELECT t.v, t.x FROM t");
        assert_eq!((r.limit, r.offset), (Some(1), 2));
    }

    #[test]
    fn an_order_by_is_kept_and_applied_per_key() {
        let r = rewrite("SELECT t.v FROM t WHERE t.x = $1 ORDER BY t.y DESC, t.v LIMIT 1")
            .expect("qualifies");
        assert_eq!(r.sql, "SELECT t.v, t.x FROM t ORDER BY t.y DESC, t.v");
        assert_eq!((r.limit, r.offset), (Some(1), 0));
    }

    #[test]
    fn sum_and_avg_under_a_filter_are_late() {
        for (sql, late) in [
            (
                "SELECT sum(t.v) FROM t WHERE t.x = $1 AND t.y > $2",
                Late::Sum,
            ),
            (
                "SELECT avg(t.v) FROM t WHERE t.x = $1 AND t.y > $2",
                Late::Avg,
            ),
        ] {
            assert_eq!(rewrite(sql).expect("qualifies").late, Some(late), "{sql}");
        }
        let one = Bson::Int32(1);
        let two = Bson::Int32(2);
        assert_eq!(
            Late::Sum.over(&[&one, &Bson::Null, &two]),
            Some(Bson::Int64(3))
        );
        assert_eq!(Late::Sum.over(&[&Bson::Null]), Some(Bson::Null));
        assert_eq!(Late::Avg.over(&[]), Some(Bson::Null));
        // Only 32-bit integers are summed here.
        assert_eq!(Late::Sum.over(&[&Bson::Int64(1)]), None);
    }

    #[test]
    fn shapes_that_cannot_run_once_are_left_alone() {
        for sql in [
            "SELECT string_agg(t.v, ',') FROM t WHERE t.x = $1 AND t.y > $2",
            "SELECT count(DISTINCT t.v) FROM t WHERE t.x = $1 AND t.y > $2",
            "SELECT max(t.v + 1) FROM t WHERE t.x = $1 AND t.y > $2",
            "SELECT 1 FROM t WHERE t.y ~ $1",
            "SELECT t.v FROM t WHERE t.x = $1 ORDER BY t.v + $1",
            "SELECT count(*) FROM t WHERE t.x = $1 GROUP BY t.y ORDER BY 1",
            "SELECT t.v FROM t WHERE t.x = $1 ORDER BY (SELECT 1)",
            "SELECT DISTINCT t.v FROM t WHERE t.x = $1",
            "SELECT random() FROM t WHERE t.x = $1",
            "SELECT string_agg(t.y, ',') FROM t WHERE t.x = $1",
            "SELECT count(*) FROM t WHERE t.x = $1 LIMIT 1",
        ] {
            assert!(rewrite(sql).is_none(), "{sql}");
        }
    }

    #[test]
    fn comparisons_become_filters() {
        let r = rewrite("SELECT 1 FROM t WHERE t.x = $1 AND $2 < t.y").expect("qualifies");
        assert_eq!(r.sql, "SELECT 1, t.x, t.y, pg_typeof(t.y)::text FROM t");
        assert_eq!(r.params, vec![0]);
        assert_eq!(r.filters.len(), 1);
        assert_eq!((r.filters[0].param, r.filters[0].op), (1, Op::Gt));
    }

    #[test]
    fn sql_order_is_numeric_and_puts_nan_last() {
        use std::cmp::Ordering::{Equal, Greater, Less};
        assert_eq!(sql_order(&Bson::Int32(2), &Bson::Double(2.5)), Some(Less));
        assert_eq!(
            sql_order(&Bson::Double(f64::NAN), &Bson::Int64(9)),
            Some(Greater)
        );
        assert_eq!(
            sql_order(&Bson::Double(f64::NAN), &Bson::Double(f64::NAN)),
            Some(Equal)
        );
        assert_eq!(sql_order(&Bson::String("a".into()), &Bson::Int32(1)), None);
        assert_eq!(
            sql_order(&Bson::String("B".into()), &Bson::String("a".into())),
            Some(Less)
        );
    }

    #[test]
    fn numerics_key_by_exact_value() {
        let d = |s: &str| key_of(&Bson::Decimal128(s.parse().expect("decimal")));
        assert_eq!(d("5"), key_of(&Bson::Int32(5)));
        assert_eq!(d("5.000"), key_of(&Bson::Int64(5)));
        assert_eq!(d("5E+1"), key_of(&Bson::Int32(50)));
        assert_eq!(d("-0.00"), key_of(&Bson::Int32(0)));
        assert_eq!(d("1.50"), d("1.5"));
        assert_ne!(d("1.5"), d("1.05"));
        assert_eq!(d("1.5"), Some(Key::Dec(false, "15".into(), -1)));
        assert_ne!(d("-1.5"), d("1.5"));
        assert_eq!(d("NaN"), key_of(&Bson::Double(f64::NAN)));
        // A float and a numeric never meet in the hash.
        let mut kinds = Kinds::default();
        kinds.note(&d("0.1").expect("key"));
        assert!(kinds.clashes(&key_of(&Bson::Double(0.1)).expect("key")));
        assert!(!kinds.clashes(&key_of(&Bson::Int32(1)).expect("key")));
    }

    #[test]
    fn an_aggregate_under_a_filter_runs_late() {
        let r = rewrite("SELECT max(t.v) FROM t WHERE t.x = $1 AND t.y > $2").expect("qualifies");
        assert_eq!(
            r.sql,
            "SELECT t.v, t.x, t.y, pg_typeof(t.y)::text, pg_typeof(t.v)::text FROM t"
        );
        assert_eq!(r.late, Some(Late::Max));
        assert!(!r.aggregate);
        let r = rewrite("SELECT count(*) FROM t WHERE t.x = $1 AND t.y > $2").expect("qualifies");
        assert_eq!(r.late, Some(Late::CountStar));
        let (a, b, n) = (Bson::Int32(3), Bson::Double(2.5), Bson::Null);
        assert_eq!(Late::Max.over(&[&a, &n, &b]), Some(Bson::Int32(3)));
        assert_eq!(Late::Min.over(&[&a, &n, &b]), Some(Bson::Double(2.5)));
        assert_eq!(Late::Min.over(&[]), Some(Bson::Null));
        assert_eq!(Late::Count.over(&[&a, &n]), Some(Bson::Int64(1)));
        assert_eq!(Late::CountStar.over(&[&a, &n]), Some(Bson::Int64(2)));
    }

    #[test]
    fn joined_tables_qualify() {
        let r =
            rewrite("SELECT 1 FROM t JOIN u ON u.tid = t.id WHERE t.x = $1").expect("qualifies");
        assert_eq!(r.sql, "SELECT 1, t.x FROM t JOIN u ON u.tid = t.id");
        assert!(
            rewrite("SELECT 1 FROM t JOIN generate_series(1, 2) g ON true WHERE t.x = $1")
                .is_none()
        );
    }

    #[test]
    fn keys_fold_integral_numbers_and_signed_zero() {
        assert_eq!(key_of(&Bson::Int32(1)), key_of(&Bson::Double(1.0)));
        assert_eq!(key_of(&Bson::Int64(0)), key_of(&Bson::Double(-0.0)));
        assert_ne!(key_of(&Bson::Double(2.5)), key_of(&Bson::Int32(2)));
        assert_eq!(key_of(&Bson::Null), None);
    }
}
