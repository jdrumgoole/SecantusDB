//! `EXISTS` / `NOT EXISTS` over one equality as a semi-join.
//!
//! A correlated subquery runs once per distinct outer value
//! (`correlated.rs`): plan, scan, repeat. For the commonest shape --
//! `WHERE EXISTS (SELECT ... FROM t WHERE t.x = o.y AND <t only>)` -- that is
//! a scan of `t` per outer row (7 s for 2,000 x 2,000 rows). PostgreSQL plans
//! it as a semi-join; here it becomes the UNCORRELATED `IN` it is equivalent
//! to, which runs once and matches through the outer side's index:
//!
//! * `EXISTS`     -> `o.y IN (SELECT t.x FROM t WHERE <t only>)`
//! * `NOT EXISTS` -> `o.y IS NULL OR o.y NOT IN (SELECT t.x FROM t WHERE
//!   t.x IS NOT NULL AND <t only>)`
//!
//! Only as a top-level conjunct of WHERE, where a NULL result means "no row"
//! exactly as false does: `o.y IN (...)` is NULL where `EXISTS` is false for
//! a NULL `o.y`, which a select-list `EXISTS` would show. The NOT form
//! excludes NULL keys from the list so `NOT IN` is never NULL.

use super::*;

/// Rewrite `s`'s WHERE conjuncts that are such an `EXISTS` / `NOT EXISTS`.
pub(crate) fn rewrite(
    s: &mut pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) {
    let mut outer = Vec::new();
    for item in &s.from_clause {
        from_aliases(item, &mut outer);
    }
    if outer.is_empty() {
        return;
    }
    let Some(w) = s.where_clause.as_deref_mut() else {
        return;
    };
    match w.node.as_mut() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in &mut b.args {
                rewrite_conjunct(a, &outer, lookup);
            }
        }
        _ => rewrite_conjunct(w, &outer, lookup),
    }
}

fn rewrite_conjunct(
    n: &mut pg_query::protobuf::Node,
    outer: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) {
    let (sublink, negated) = match n.node.as_ref() {
        Some(N::SubLink(sl)) => (sl.as_ref(), false),
        Some(N::BoolExpr(b))
            if b.boolop == pg_query::protobuf::BoolExprType::NotExpr as i32
                && b.args.len() == 1 =>
        {
            match b.args[0].node.as_ref() {
                Some(N::SubLink(sl)) => (sl.as_ref(), true),
                _ => return,
            }
        }
        _ => return,
    };
    if sublink.sub_link_type != pg_query::protobuf::SubLinkType::ExistsSublink as i32 {
        return;
    }
    let Some(Some(N::SelectStmt(sub))) = sublink.subselect.as_deref().map(|n| n.node.as_ref())
    else {
        return;
    };
    if let Some(replacement) = semi_join(sub, negated, outer, lookup) {
        *n = replacement;
    }
}

/// The IN form of `EXISTS (sub)`, when `sub` has the one-equality shape.
fn semi_join(
    sub: &pg_query::protobuf::SelectStmt,
    negated: bool,
    outer: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<pg_query::protobuf::Node> {
    // One plain table, and nothing that makes the row count differ from the
    // WHERE's: an aggregate (`EXISTS (SELECT count(*) ...)` is always true),
    // grouping, DISTINCT, LIMIT / OFFSET, a set operation, a CTE.
    if sub.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || sub.from_clause.len() != 1
        || !sub.group_clause.is_empty()
        || sub.having_clause.is_some()
        || !sub.distinct_clause.is_empty()
        || sub.limit_count.is_some()
        || sub.limit_offset.is_some()
        || sub.with_clause.is_some()
        || !sub.window_clause.is_empty()
        || !sub.values_lists.is_empty()
        || sub
            .target_list
            .iter()
            .any(|t| contains_node(t, &|n| matches!(n, N::FuncCall(_) | N::SubLink(_))))
    {
        return None;
    }
    let Some(N::RangeVar(rv)) = sub.from_clause[0].node.as_ref() else {
        return None;
    };
    let inner_alias = rv
        .alias
        .as_ref()
        .filter(|a| a.colnames.is_empty())
        .map(|a| a.aliasname.clone())
        .or_else(|| rv.alias.is_none().then(|| rv.relname.clone()))?;
    if outer.contains(&inner_alias) {
        return None;
    }
    let def = lookup(&relation_name(rv))?;
    let mut conjuncts = Vec::new();
    split_and(sub.where_clause.as_deref()?, &mut conjuncts);
    // Exactly one conjunct reads the outer query: `inner = outer`.
    let mut link: Option<(pg_query::protobuf::Node, pg_query::protobuf::Node)> = None;
    let mut rest = Vec::new();
    for c in conjuncts {
        if contains_node(c, &|n| matches!(n, N::SubLink(_))) {
            return None;
        }
        match classify(c, &inner_alias, &def, outer) {
            Side::Inner => rest.push(c.clone()),
            Side::Outer => return None,
            Side::Link(pair) => {
                if link.is_some() {
                    return None;
                }
                link = Some(*pair);
            }
        }
    }
    let (inner_ref, outer_ref) = link?;
    if negated {
        rest.push(null_test(inner_ref.clone(), false));
    }
    let list = pg_query::protobuf::SelectStmt {
        target_list: vec![pg_query::protobuf::Node {
            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                val: Some(Box::new(inner_ref)),
                location: -1,
                ..Default::default()
            }))),
        }],
        from_clause: sub.from_clause.clone(),
        where_clause: and_of(rest).map(Box::new),
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        ..Default::default()
    };
    let in_list = pg_query::protobuf::Node {
        node: Some(N::SubLink(Box::new(pg_query::protobuf::SubLink {
            sub_link_type: pg_query::protobuf::SubLinkType::AnySublink as i32,
            testexpr: Some(Box::new(outer_ref.clone())),
            subselect: Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::SelectStmt(Box::new(list))),
            })),
            location: -1,
            ..Default::default()
        }))),
    };
    if !negated {
        return Some(in_list);
    }
    let not_in = bool_expr(pg_query::protobuf::BoolExprType::NotExpr, vec![in_list]);
    Some(bool_expr(
        pg_query::protobuf::BoolExprType::OrExpr,
        vec![null_test(outer_ref, true), not_in],
    ))
}

enum Side {
    /// Reads only the inner table (or nothing).
    Inner,
    /// Reads the outer query some other way.
    Outer,
    /// `inner = outer`, as (inner, outer).
    Link(Box<(pg_query::protobuf::Node, pg_query::protobuf::Node)>),
}

/// Whose columns a column reference names: `Some(true)` the inner table,
/// `Some(false)` an outer FROM item, `None` neither (leave it alone).
fn owner(
    c: &pg_query::protobuf::ColumnRef,
    inner: &str,
    def: &TableDef,
    outer: &[String],
) -> Option<bool> {
    let parts: Vec<&str> = c
        .fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [col] => def.column(col).is_some().then_some(true),
        [q, col] if *q == inner => def.column(col).is_some().then_some(true),
        [q, _] if outer.iter().any(|o| o == q) => Some(false),
        _ => None,
    }
}

fn classify(c: &pg_query::protobuf::Node, inner: &str, def: &TableDef, outer: &[String]) -> Side {
    // `inner = outer` in either order.
    if let Some(N::AExpr(e)) = c.node.as_ref() {
        let is_eq = e.kind == pg_query::protobuf::AExprKind::AexprOp as i32
            && matches!(e.name.as_slice(), [n] if matches!(n.node.as_ref(), Some(N::String(s)) if s.sval == "="));
        if is_eq {
            if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                let side = |n: &pg_query::protobuf::Node| match n.node.as_ref() {
                    Some(N::ColumnRef(cr)) => owner(cr, inner, def, outer),
                    _ => None,
                };
                match (side(l), side(r)) {
                    (Some(true), Some(false)) => {
                        return Side::Link(Box::new((l.clone(), r.clone())))
                    }
                    (Some(false), Some(true)) => {
                        return Side::Link(Box::new((r.clone(), l.clone())))
                    }
                    _ => {}
                }
            }
        }
    }
    // Any column that is not the inner table's (an outer one, or one this
    // cannot place) keeps the subquery on the per-row path.
    let foreign = contains_node(
        c,
        &|n| matches!(n, N::ColumnRef(cr) if owner(cr, inner, def, outer) != Some(true)),
    );
    if foreign {
        Side::Outer
    } else {
        Side::Inner
    }
}

/// Does any node below `n` satisfy `f`?
fn contains_node(n: &pg_query::protobuf::Node, f: &dyn Fn(&N) -> bool) -> bool {
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

fn split_and<'a>(n: &'a pg_query::protobuf::Node, out: &mut Vec<&'a pg_query::protobuf::Node>) {
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in &b.args {
                split_and(a, out);
            }
        }
        _ => out.push(n),
    }
}

fn bool_expr(
    op: pg_query::protobuf::BoolExprType,
    args: Vec<pg_query::protobuf::Node>,
) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
            boolop: op as i32,
            args,
            location: -1,
            ..Default::default()
        }))),
    }
}

fn and_of(mut items: Vec<pg_query::protobuf::Node>) -> Option<pg_query::protobuf::Node> {
    match items.len() {
        0 => None,
        1 => items.pop(),
        _ => Some(bool_expr(pg_query::protobuf::BoolExprType::AndExpr, items)),
    }
}

fn null_test(arg: pg_query::protobuf::Node, is_null: bool) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::NullTest(Box::new(pg_query::protobuf::NullTest {
            arg: Some(Box::new(arg)),
            nulltesttype: if is_null {
                pg_query::protobuf::NullTestType::IsNull as i32
            } else {
                pg_query::protobuf::NullTestType::IsNotNull as i32
            },
            location: -1,
            ..Default::default()
        }))),
    }
}

/// The names this query's FROM items are referenced by.
fn from_aliases(item: &pg_query::protobuf::Node, out: &mut Vec<String>) {
    match item.node.as_ref() {
        Some(N::RangeVar(r)) => out.push(
            r.alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_else(|| r.relname.clone()),
        ),
        Some(N::RangeSubselect(s)) => out.extend(s.alias.as_ref().map(|a| a.aliasname.clone())),
        Some(N::RangeFunction(f)) => out.extend(f.alias.as_ref().map(|a| a.aliasname.clone())),
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                from_aliases(side, out);
            }
        }
        _ => {}
    }
}
