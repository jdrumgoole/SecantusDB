//! A value is sorted only by its type's default btree operator class, and
//! grouped or de-duplicated only by its btree or hash class. A type with
//! neither (`point`, `json`, `xml`, the geometric types) is refused when the
//! statement is analysed: `42883 could not identify an ordering operator for
//! type point` under ORDER BY, and `... an equality operator ...` under
//! DISTINCT, GROUP BY, PARTITION BY and a set operation that is not ALL.
//! Lowered to MQL those would instead sort or compare the stored values and
//! answer rows PostgreSQL never returns.
//!
//! The error is at the expression as written (`order by 1` at the `1`); the
//! checks run in parse analysis' order -- ORDER BY, GROUP BY, DISTINCT --
//! so a query with two such clauses reports the first. Measured on 15.19.

use super::*;

/// The `pg_catalog` types with no default btree operator class: no
/// ORDER BY (from `select x from (select null::T x) s order by x` over every
/// base type in `pg_type`, on 15.19).
const NO_ORDERING: &[&str] = &[
    "aclitem",
    "box",
    "cid",
    "circle",
    "gtsvector",
    "json",
    "jsonpath",
    "line",
    "lseg",
    "path",
    "pg_brin_bloom_summary",
    "pg_brin_minmax_multi_summary",
    "pg_snapshot",
    "point",
    "polygon",
    "refcursor",
    "txid_snapshot",
    "xid",
    "xml",
];

/// The `pg_catalog` types with neither a btree nor a hash class: no
/// DISTINCT or GROUP BY (`select distinct null::T`, the same sweep).
/// `aclitem`, `cid` and `xid` hash.
const NO_EQUALITY: &[&str] = &[
    "box",
    "circle",
    "gtsvector",
    "json",
    "jsonpath",
    "line",
    "lseg",
    "path",
    "pg_brin_bloom_summary",
    "pg_brin_minmax_multi_summary",
    "pg_snapshot",
    "point",
    "polygon",
    "refcursor",
    "txid_snapshot",
    "xml",
];

#[derive(Clone, Copy)]
enum Need {
    Ordering,
    Equality,
}

/// Refuse `n` (typed `ty`) when its type, or an array's element type, has
/// no operator for `need`.
fn require(n: &pg_query::protobuf::Node, ty: Option<String>, need: Need) -> Result<()> {
    let Some(ty) = ty else {
        return Ok(());
    };
    let (list, word) = match need {
        Need::Ordering => (NO_ORDERING, "ordering"),
        Need::Equality => (NO_EQUALITY, "equality"),
    };
    let base = ty.trim_end_matches("[]");
    let base = base.strip_prefix("pg_catalog.").unwrap_or(base);
    if !list.contains(&base) {
        return Ok(());
    }
    if let Some(loc) = expr_location(n) {
        set_error_location(loc);
    }
    Err(Error::UndefinedFunction(format!(
        "could not identify an {word} operator for type {}",
        display_type(&ty)
    )))
}

/// The operator an `ORDER BY ... USING op` names, without its schema.
pub(crate) fn using_operator(b: &pg_query::protobuf::SortBy) -> Option<String> {
    b.use_op.last().and_then(|n| match n.node.as_ref() {
        Some(N::String(s)) => Some(s.sval.clone()),
        _ => None,
    })
}

/// The sort direction `USING op` stands for: the `<` / `>` members of a
/// btree family (`text_pattern_ops`' `~<~` / `~>~` too), `true` for
/// ascending. Any other operator -- even `<=` -- is not an ordering
/// operator (measured on 15.19).
pub(crate) fn using_ascending(op: &str) -> Option<bool> {
    match op {
        "<" | "~<~" => Some(true),
        ">" | "~>~" => Some(false),
        _ => None,
    }
}

/// One sort item typed `ty`: plain, it needs an ordering operator; with
/// `USING op`, `op` must exist for the type (42883 `operator does not
/// exist: point < point`) and be an ordering one (42809), both at the
/// operator.
fn sort_item(
    b: &pg_query::protobuf::SortBy,
    n: &pg_query::protobuf::Node,
    ty: Option<String>,
) -> Result<()> {
    let Some(op) = using_operator(b) else {
        return require(n, ty, Need::Ordering);
    };
    if let Some(t) = &ty {
        if crate::funcsig::operator_resolves(&op, t, t) == Some(false) {
            return Err(mismatch(&op, t, t, b.location));
        }
    }
    if using_ascending(&op).is_none() {
        set_error_location(b.location);
        return Err(Error::Sqlstate(
            "42809",
            format!("operator {op} is not a valid ordering operator"),
        ));
    }
    Ok(())
}

fn res_target(t: &pg_query::protobuf::Node) -> Option<&pg_query::protobuf::ResTarget> {
    match t.node.as_ref() {
        Some(N::ResTarget(rt)) => Some(rt),
        _ => None,
    }
}

/// An ORDER BY / GROUP BY item that names an output column: an ordinal, or
/// (ORDER BY first, GROUP BY only when no input column has it) a bare name
/// a target is called by.
fn output_column<'s>(
    targets: &'s [pg_query::protobuf::Node],
    n: &pg_query::protobuf::Node,
) -> Option<&'s pg_query::protobuf::Node> {
    match n.node.as_ref()? {
        N::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Ival(i) => {
                let k = usize::try_from(i.ival).ok()?.checked_sub(1)?;
                res_target(targets.get(k)?)?.val.as_deref()
            }
            _ => None,
        },
        N::ColumnRef(c) => {
            let [f] = c.fields.as_slice() else {
                return None;
            };
            let Some(N::String(s)) = f.node.as_ref() else {
                return None;
            };
            targets
                .iter()
                .filter_map(res_target)
                .find(|rt| rt.name == s.sval)
                .and_then(|rt| rt.val.as_deref())
        }
        _ => None,
    }
}

fn typed(n: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    operand_type(n, scope).filter(|t| !t.is_empty() && t != "unknown")
}

/// ORDER BY, GROUP BY, DISTINCT [ON] and the named windows of one SELECT
/// over its own FROM.
pub(super) fn check_clauses(s: &pg_query::protobuf::SelectStmt, scope: &Scope) -> Result<()> {
    let targets = &s.target_list;
    for b in s.sort_clause.iter().filter_map(|n| match n.node.as_ref() {
        Some(N::SortBy(b)) => Some(b),
        _ => None,
    }) {
        let Some(n) = b.node.as_deref() else {
            continue;
        };
        let ty = output_column(targets, n)
            .and_then(|v| typed(v, scope))
            .or_else(|| typed(n, scope));
        sort_item(b, n, ty)?;
    }
    for n in &s.group_clause {
        let ordinal = matches!(n.node.as_ref(), Some(N::AConst(_)));
        let output = || output_column(targets, n).and_then(|v| typed(v, scope));
        let ty = if ordinal {
            output()
        } else {
            typed(n, scope).or_else(output)
        };
        require(n, ty, Need::Equality)?;
    }
    match s.distinct_clause.as_slice() {
        [] => {}
        [only] if only.node.is_none() => {
            for v in targets
                .iter()
                .filter_map(res_target)
                .filter_map(|rt| rt.val.as_deref())
            {
                require(v, typed(v, scope), Need::Equality)?;
            }
        }
        on => {
            for n in on {
                require(n, typed(n, scope), Need::Equality)?;
            }
        }
    }
    for w in &s.window_clause {
        if let Some(N::WindowDef(w)) = w.node.as_ref() {
            check_window(w, scope)?;
        }
    }
    Ok(())
}

/// A window's PARTITION BY needs equality and its ORDER BY an ordering.
pub(super) fn check_window(w: &pg_query::protobuf::WindowDef, scope: &Scope) -> Result<()> {
    for n in &w.partition_clause {
        require(n, typed(n, scope), Need::Equality)?;
    }
    sort_items(&w.order_clause, scope)
}

fn sort_items(items: &[pg_query::protobuf::Node], scope: &Scope) -> Result<()> {
    for b in items.iter().filter_map(|n| match n.node.as_ref() {
        Some(N::SortBy(b)) => Some(b),
        _ => None,
    }) {
        if let Some(n) = b.node.as_deref() {
            sort_item(b, n, typed(n, scope))?;
        }
    }
    Ok(())
}

/// An aggregate's own ORDER BY, and the argument of `agg(DISTINCT x)`.
pub(super) fn check_call(f: &pg_query::protobuf::FuncCall, scope: &Scope) -> Result<()> {
    if f.agg_distinct {
        for a in &f.args {
            require(a, typed(a, scope), Need::Equality)?;
        }
    }
    sort_items(&f.agg_order, scope)
}

/// The leaf SELECTs of a set operation, left to right.
fn leaves<'s>(
    s: &'s pg_query::protobuf::SelectStmt,
    out: &mut Vec<&'s pg_query::protobuf::SelectStmt>,
) {
    if s.op == pg_query::protobuf::SetOperation::SetopNone as i32 {
        out.push(s);
        return;
    }
    for arm in [s.larg.as_deref(), s.rarg.as_deref()].into_iter().flatten() {
        leaves(arm, out);
    }
}

/// A set operation other than UNION ALL compares rows, so each column needs
/// equality, reported at the first arm whose column has a
/// type (`select null union select p` is at the `p`). Then its ORDER BY,
/// over the first arm's output names.
pub(super) fn check_set_operation(
    s: &pg_query::protobuf::SelectStmt,
    ctes: &[Cte],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    parent: Option<&Scope>,
) -> Result<()> {
    let mut arms = Vec::new();
    leaves(s, &mut arms);
    if arms.iter().any(|a| a.target_list.is_empty()) {
        return Ok(());
    }
    let scopes: Vec<Scope> = arms
        .iter()
        .map(|a| Scope::new(&a.from_clause, ctes, lookup, parent))
        .collect();
    // UNION ALL is the one that never compares; INTERSECT ALL and EXCEPT
    // ALL still match rows.
    if !(s.all && s.op == pg_query::protobuf::SetOperation::SetopUnion as i32) {
        let width = arms[0].target_list.len();
        for k in 0..width {
            let mut first: Option<(&pg_query::protobuf::Node, String)> = None;
            let mut agree = true;
            for (arm, scope) in arms.iter().zip(&scopes) {
                let Some(v) = arm
                    .target_list
                    .get(k)
                    .and_then(res_target)
                    .and_then(|rt| rt.val.as_deref())
                else {
                    agree = false;
                    break;
                };
                if let Some(t) = typed(v, scope) {
                    match &first {
                        None => first = Some((v, t)),
                        Some((_, f)) if *f != t => agree = false,
                        _ => {}
                    }
                }
            }
            if let (true, Some((v, t))) = (agree, first) {
                require(v, Some(t), Need::Equality)?;
            }
        }
    }
    for b in s.sort_clause.iter().filter_map(|n| match n.node.as_ref() {
        Some(N::SortBy(b)) => Some(b),
        _ => None,
    }) {
        if let Some(n) = b.node.as_deref() {
            let ty = output_column(&arms[0].target_list, n).and_then(|v| typed(v, &scopes[0]));
            sort_item(b, n, ty)?;
        }
    }
    Ok(())
}

/// `ORDER BY x USING <` as `ORDER BY x ASC` (`>` as `DESC`), everywhere in
/// the statement -- a query, an aggregate's ORDER BY, a window's -- once
/// [`check`](super::check) has refused the operators that do not order.
/// The NULLS default follows the direction, as it does for USING.
pub(crate) fn using_to_direction(node: &mut N) {
    use pg_query::protobuf::SortByDir;
    let direct = |b: &mut pg_query::protobuf::SortBy| {
        if let Some(asc) = using_operator(b).as_deref().and_then(using_ascending) {
            b.sortby_dir = if asc {
                SortByDir::SortbyAsc
            } else {
                SortByDir::SortbyDesc
            } as i32;
            b.use_op.clear();
        }
    };
    let items = |list: &mut [pg_query::protobuf::Node]| {
        for n in list {
            if let Some(N::SortBy(b)) = n.node.as_mut() {
                direct(b);
            }
        }
    };
    // SAFETY: only plain fields of SortBy nodes change; no node is replaced
    // or freed, so every pointer in the list stays valid. An aggregate's
    // ORDER BY and a window's are reached through their call / definition.
    unsafe {
        for (n, _, _) in node.nodes_mut() {
            match n {
                pg_query::NodeMut::SortBy(b) => direct(&mut *b),
                pg_query::NodeMut::FuncCall(f) => {
                    let f = &mut *f;
                    items(&mut f.agg_order);
                    if let Some(w) = f.over.as_deref_mut() {
                        items(&mut w.order_clause);
                    }
                }
                pg_query::NodeMut::WindowDef(w) => items(&mut (*w).order_clause),
                pg_query::NodeMut::SelectStmt(sel) => {
                    for w in &mut (*sel).window_clause {
                        if let Some(N::WindowDef(w)) = w.node.as_mut() {
                            items(&mut w.order_clause);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
