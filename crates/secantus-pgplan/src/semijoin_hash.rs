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
//! Only shapes whose rows are a pure filter of the scan qualify: no
//! aggregate, grouping, DISTINCT, LIMIT / OFFSET, ORDER BY, window, set
//! operation, CTE, locking clause, nested subquery or function in the select
//! list (a volatile one could not be run once). And only key values whose
//! hash equality IS SQL equality are hashed: integers, floats folded to their
//! numeric value, text, booleans, dates and timestamps. Anything else -- a
//! numeric, a collation, a mixed-type comparison, NULL -- falls back to the
//! per-row path, which keeps every answer it gave before.

use super::*;
use std::collections::HashMap;

/// A key value under SQL equality: integral numbers fold together across
/// int / bigint / float, so `1 = 1.0` hashes alike.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Key {
    Int(i64),
    Float(u64),
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
            Key::Int(_) | Key::Float(_) | Key::NaN => Family::Number,
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
        _ => None,
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
    /// `LIMIT` / `OFFSET`, applied within each key's rows (scan order, as
    /// the per-row query would see them).
    limit: Option<usize>,
    offset: usize,
    families: Vec<Option<Family>>,
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

#[derive(Clone, Copy, Debug)]
struct Filter {
    param: usize,
    op: Op,
}

/// SQL's ordering of two numbers or two timestamps, or `None` for anything
/// else (text, numeric, mixed families, NULL) -- which the per-row path then
/// answers. NaN sorts above every number and equals NaN, as in PostgreSQL.
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
}

/// Enables the cache for one statement's execution and drops it after.
pub(crate) struct Scope(Option<HashMap<String, Entry>>);

impl Scope {
    pub(crate) fn enter() -> Self {
        Scope(CACHE.with(|c| c.replace(Some(HashMap::new()))))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        CACHE.with(|c| *c.borrow_mut() = self.0.take());
    }
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
            if ix.families[i].is_some_and(|f| f != k.family()) {
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
                return Ok(Some(Vec::new()));
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
                Ok(Some(
                    out.into_iter()
                        .skip(ix.offset)
                        .take(ix.limit.unwrap_or(usize::MAX))
                        .collect(),
                ))
            }
            None if ix.aggregate => Ok(None),
            None => Ok(Some(Vec::new())),
        }
    })
}

fn build(sql: &str, run: Runner) -> Result<Entry> {
    let Some(Rewritten {
        sql: rewritten,
        params,
        filters,
        aggregate,
        limit,
        offset,
    }) = rewrite(sql)
    else {
        return Ok(Entry::No);
    };
    let rows = run(&rewritten, &[])?;
    let nkeys = params.len();
    let mut families: Vec<Option<Family>> = vec![None; nkeys];
    let nfilters = filters.len();
    let mut index: Groups = HashMap::new();
    for mut row in rows {
        if row.len() < nkeys + nfilters {
            return Ok(Entry::No);
        }
        let fvals = row.split_off(row.len() - nfilters);
        // Text, numeric and anything else unorderable here: per-row path.
        if fvals.iter().any(|v| {
            !matches!(
                v,
                Bson::Null | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) | Bson::DateTime(_)
            )
        }) {
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
            key.push(k);
        }
        if !null {
            index.entry(key).or_default().push((row, fvals));
        }
    }
    Ok(Entry::Built(Index {
        params,
        aggregate,
        limit,
        offset,
        families,
        filters,
        rows: index,
    }))
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
        || !s.sort_clause.is_empty()
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
        }) || !matches!(f.node.as_ref(), Some(N::RangeVar(_)))
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
    // grouping by the keys alone cannot express.
    if aggregate && !filters.is_empty() {
        return None;
    }
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
    for (col, f) in filters {
        s.target_list.push(pg_query::protobuf::Node {
            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                val: Some(Box::new(col)),
                location: -1,
                ..Default::default()
            }))),
        });
        filter_list.push(f);
    }
    let text = pg_query::deparse(&parsed).ok()?;
    Some(Rewritten {
        sql: text,
        params,
        filters: filter_list,
        aggregate,
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
    fn shapes_that_cannot_run_once_are_left_alone() {
        for sql in [
            "SELECT count(*) FROM t WHERE t.x = $1 AND t.y > $2",
            "SELECT 1 FROM t WHERE t.y ~ $1",
            "SELECT 1 FROM t WHERE t.x = $1 ORDER BY t.v",
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
        assert_eq!(r.sql, "SELECT 1, t.x, t.y FROM t");
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
    }

    #[test]
    fn keys_fold_integral_numbers_and_signed_zero() {
        assert_eq!(key_of(&Bson::Int32(1)), key_of(&Bson::Double(1.0)));
        assert_eq!(key_of(&Bson::Int64(0)), key_of(&Bson::Double(-0.0)));
        assert_ne!(key_of(&Bson::Double(2.5)), key_of(&Bson::Int32(2)));
        assert_eq!(key_of(&Bson::Null), None);
    }
}
