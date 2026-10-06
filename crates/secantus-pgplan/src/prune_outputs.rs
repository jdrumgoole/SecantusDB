//! PostgreSQL never computes a FROM-subquery output that nothing reads.
//!
//! Its planner either pulls a simple subquery up into its parent (so an
//! unread output simply disappears) or, for one it cannot pull up, replaces
//! every unread output with a NULL constant (`remove_unused_subquery_outputs`
//! in `allpaths.c`). So `select count(*) from (select a/b from t) s` answers
//! even when `b` is zero: `a/b` never runs.
//!
//! [`rewrite`] does the second over the parse tree, which covers both: an
//! output of a FROM-subquery that no reference in the enclosing SELECT can
//! name becomes `NULL`. PostgreSQL's exceptions are kept, and so is anything
//! this server cannot classify:
//!
//! - nothing is removed under a plain DISTINCT, or in a set operation other
//!   than UNION ALL (whose arms PostgreSQL pulls up one by one);
//! - an output named by the subquery's own ORDER BY / GROUP BY / DISTINCT ON
//!   / WINDOW (by name or position) stays (`ressortgroupref`);
//! - an output holding a volatile or set-returning call stays -- here that is
//!   any call that is not an IMMUTABLE built-in, a built-in aggregate or a
//!   window call, so a STABLE built-in or any user function is kept too;
//! - a whole-row reference (`s`, `s.*`, `*`), a NATURAL join or a join alias
//!   with column names keeps every output;
//! - removing an aggregate that would leave a GROUP BY-less query with none
//!   (so returning one row per input row instead of one) is not done.
//!
//! Outputs are matched to references by NAME, over every column reference in
//! the enclosing SELECT (its subqueries and LATERAL siblings included), so a
//! name that also belongs to another table keeps the output: conservative,
//! never a missing column.

use super::*;
use std::collections::HashSet;

/// What a trial plan of a subquery body needs.
struct Ctx<'a> {
    lookup: &'a dyn Fn(&str) -> Option<TableDef>,
    params: &'a [Bson],
}

/// Rewrite every `SELECT` in `node`.
pub(crate) fn rewrite(
    node: &mut pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) {
    let ctx = Ctx { lookup, params };
    if let Some(N::SelectStmt(sel)) = node.node.as_mut() {
        rewrite_select(sel, &ctx);
    }
}

/// Does `body` plan on its own? An output is removed only from a body that
/// does: PostgreSQL's parse analysis rejects an unknown column, a bad
/// reference or a grouping error in an output nothing reads before its
/// planner removes anything, and a body that cannot be planned alone (one
/// reading an outer query, a CTE, a subquery the planner runs) keeps every
/// output, so the statement fails or answers exactly as before. Nothing the
/// trial plan raises or records survives it.
fn plans_alone(body: &pg_query::protobuf::SelectStmt, ctx: &Ctx<'_>) -> bool {
    let joins = crate::joins::planned_joins_len();
    let warnings = crate::warnings_len();
    let notices = crate::fts::notices_len();
    let ok = crate::correlated::without_side_effects(|| {
        crate::plan_node(
            N::SelectStmt(Box::new(body.clone())),
            ctx.lookup,
            ctx.params,
        )
    })
    .is_ok();
    crate::joins::truncate_planned_joins(joins);
    crate::truncate_warnings(warnings);
    crate::fts::truncate_notices(notices);
    ok
}

/// `plans_alone` for a trial FROM list holding a LATERAL body: planning a
/// lateral join evaluates its body over NULL stand-ins for the left side,
/// so a DATA error (class 22, `s.a / 0`) there is evaluation, not the
/// parse analysis this check is for, and does not count.
fn lateral_plans(trial: &pg_query::protobuf::SelectStmt, ctx: &Ctx<'_>) -> bool {
    let joins = crate::joins::planned_joins_len();
    let warnings = crate::warnings_len();
    let notices = crate::fts::notices_len();
    let out = crate::correlated::without_side_effects(|| {
        crate::plan_node(
            N::SelectStmt(Box::new(trial.clone())),
            ctx.lookup,
            ctx.params,
        )
    });
    crate::joins::truncate_planned_joins(joins);
    crate::truncate_warnings(warnings);
    crate::fts::truncate_notices(notices);
    match out {
        Ok(_) => true,
        Err(e) => e.sqlstate().starts_with("22"),
    }
}

fn rewrite_select(sel: &mut pg_query::protobuf::SelectStmt, ctx: &Ctx<'_>) {
    for side in [sel.larg.as_deref_mut(), sel.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        rewrite_select(side, ctx);
    }
    prune_inlined_ctes(sel, ctx);
    // To a fixpoint: an output removed from one FROM item can be the last
    // reader of another's (`(...) s, lateral (select s.x) l`).
    let n = count_from_subqueries(&sel.from_clause);
    for _ in 0..=n {
        let mut changed = false;
        for i in 0..n {
            changed |= prune_one(sel, i, ctx);
        }
        if !changed {
            break;
        }
    }
    // Then inside each FROM-subquery, CTE and expression subquery.
    for item in &mut sel.from_clause {
        rewrite_from(item, ctx);
    }
    if let Some(with) = sel.with_clause.as_mut() {
        for cte in &mut with.ctes {
            if let Some(N::CommonTableExpr(c)) = cte.node.as_mut() {
                if let Some(N::SelectStmt(body)) =
                    c.ctequery.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    rewrite_select(body, ctx);
                }
            }
        }
    }
    for n in sel
        .target_list
        .iter_mut()
        .chain(sel.where_clause.as_deref_mut())
        .chain(sel.having_clause.as_deref_mut())
    {
        let _ = walk_expr(n, &mut |node| {
            if let Some(N::SubLink(sl)) = node.node.as_mut() {
                if let Some(N::SelectStmt(body)) =
                    sl.subselect.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    rewrite_select(body, ctx);
                }
            }
            Ok(())
        });
    }
}

/// PostgreSQL inlines a non-recursive, not-MATERIALIZED CTE read once whose
/// body has no volatile call (`inline_cte`), and the body is then a
/// FROM-subquery like any other. Only a reference in this SELECT's own FROM
/// is handled; anything else keeps the body whole.
fn prune_inlined_ctes(sel: &mut pg_query::protobuf::SelectStmt, ctx: &Ctx<'_>) {
    let Some(with) = sel.with_clause.as_ref() else {
        return;
    };
    if with.recursive {
        return;
    }
    let debug = format!("{sel:?}");
    for k in 0..with.ctes.len() {
        let Some(with) = sel.with_clause.as_mut() else {
            return;
        };
        let Some(N::CommonTableExpr(cte)) = with.ctes[k].node.as_ref() else {
            continue;
        };
        if cte.ctematerialized == pg_query::protobuf::CteMaterialize::Always as i32 {
            continue;
        }
        let name = cte.ctename.clone();
        // Every RangeVar of this name anywhere, schema-qualified or not: an
        // over-count only keeps the body whole.
        if debug.matches(&format!("relname: {name:?}")).count() != 1 {
            continue;
        }
        let cte_cols: Vec<String> = cte
            .aliascolnames
            .iter()
            .filter_map(|c| match c.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect();
        let taken = std::mem::take(&mut sel.with_clause);
        let found = sel.from_clause.iter().find_map(|i| cte_ref(i, &name));
        if let Some((alias, colnames)) = found {
            let colnames = if colnames.is_empty() {
                cte_cols
            } else {
                colnames
            };
            let used = used_names(sel, &alias);
            sel.with_clause = taken;
            let Some(with) = sel.with_clause.as_mut() else {
                return;
            };
            if let Some(N::CommonTableExpr(cte)) = with.ctes[k].node.as_mut() {
                if let Some(N::SelectStmt(body)) =
                    cte.ctequery.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    if !used.all && cte_body_inlinable(body) {
                        if let Some(names) = output_names(body, &colnames) {
                            let keep: Vec<bool> = names
                                .iter()
                                .map(|n| n.as_ref().is_none_or(|n| used.names.contains(n)))
                                .collect();
                            prune_checked(body, &keep, ctx);
                        }
                    }
                }
            }
        } else {
            sel.with_clause = taken;
        }
    }
}

/// The alias and column aliases of a plain reference to `name` in a FROM
/// item (its joins included).
fn cte_ref(item: &pg_query::protobuf::Node, name: &str) -> Option<(String, Vec<String>)> {
    match item.node.as_ref()? {
        N::RangeVar(rv) if rv.schemaname.is_empty() && rv.relname == name => {
            let alias = rv.alias.as_ref();
            Some((
                alias.map_or_else(|| name.to_string(), |a| a.aliasname.clone()),
                alias
                    .map(|a| {
                        a.colnames
                            .iter()
                            .filter_map(|c| match c.node.as_ref() {
                                Some(N::String(s)) => Some(s.sval.clone()),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            ))
        }
        N::JoinExpr(j) => [j.larg.as_deref(), j.rarg.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|i| cte_ref(i, name)),
        _ => None,
    }
}

/// A CTE body PostgreSQL inlines: a SELECT with no volatile call anywhere.
fn cte_body_inlinable(body: &pg_query::protobuf::SelectStmt) -> bool {
    fn arm(b: &pg_query::protobuf::SelectStmt) -> bool {
        match (b.larg.as_deref(), b.rarg.as_deref()) {
            (Some(l), Some(r)) => arm(l) && arm(r),
            _ => sublink_removable(b),
        }
    }
    arm(body)
}

fn rewrite_from(item: &mut pg_query::protobuf::Node, ctx: &Ctx<'_>) {
    match item.node.as_mut() {
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                rewrite_from(side, ctx);
            }
        }
        Some(N::RangeSubselect(rs)) => {
            if let Some(N::SelectStmt(body)) =
                rs.subquery.as_deref_mut().and_then(|q| q.node.as_mut())
            {
                rewrite_select(body, ctx);
            }
        }
        _ => {}
    }
}

fn count_from_subqueries(items: &[pg_query::protobuf::Node]) -> usize {
    items.iter().map(count_in_item).sum()
}

fn count_in_item(item: &pg_query::protobuf::Node) -> usize {
    match item.node.as_ref() {
        Some(N::JoinExpr(j)) => [j.larg.as_deref(), j.rarg.as_deref()]
            .into_iter()
            .flatten()
            .map(count_in_item)
            .sum(),
        Some(N::RangeSubselect(_)) => 1,
        _ => 0,
    }
}

/// The `i`th FROM-subquery of `items`, in a depth-first walk of the joins.
fn nth_subquery(
    items: &mut [pg_query::protobuf::Node],
    i: usize,
) -> Option<&mut pg_query::protobuf::RangeSubselect> {
    fn go<'a>(
        item: &'a mut pg_query::protobuf::Node,
        i: &mut usize,
    ) -> Option<&'a mut pg_query::protobuf::RangeSubselect> {
        match item.node.as_mut() {
            Some(N::JoinExpr(j)) => {
                if let Some(l) = j.larg.as_deref_mut() {
                    if let Some(found) = go(l, i) {
                        return Some(found);
                    }
                }
                j.rarg.as_deref_mut().and_then(|r| go(r, i))
            }
            Some(N::RangeSubselect(rs)) => {
                if *i == 0 {
                    Some(rs)
                } else {
                    *i -= 1;
                    None
                }
            }
            _ => None,
        }
    }
    let mut i = i;
    for item in items {
        if let Some(found) = go(item, &mut i) {
            return Some(found);
        }
    }
    None
}

/// What the enclosing SELECT reads of one FROM-subquery.
struct Used {
    all: bool,
    names: HashSet<String>,
}

/// Names referenced anywhere in `sel` (with the subquery being pruned taken
/// out of it), and whether a reference can read every output of `alias`.
fn used_names(sel: &pg_query::protobuf::SelectStmt, alias: &str) -> Used {
    let mut used = Used {
        all: false,
        names: HashSet::new(),
    };
    let mut copy = sel.clone();
    let _ = crate::correlated::walk_select(&mut copy, &mut |node| {
        if !walked_kind(node) {
            used.all = true;
        }
        if let Some(N::SubLink(sl)) = node.node.as_ref() {
            if let Some(N::SelectStmt(b)) = sl.subselect.as_deref().and_then(|q| q.node.as_ref()) {
                if b.from_clause.iter().any(|f| !plain_from(f)) {
                    used.all = true;
                }
            }
        }
        if let Some(N::ColumnRef(c)) = node.node.as_ref() {
            let fields: Vec<Option<&str>> = c
                .fields
                .iter()
                .map(|f| match f.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.as_str()),
                    _ => None,
                })
                .collect();
            if fields.iter().any(|f| f.is_none()) || (fields.len() == 1 && fields[0] == Some(alias))
            {
                used.all = true;
            }
            match fields.as_slice() {
                // `q.c` with `q` another relation is not a reference to this
                // one (a composite column's field needs `(q).c`).
                [Some(q), Some(_)] if *q != alias => {}
                _ => {
                    for f in fields.into_iter().flatten() {
                        used.names.insert(f.to_string());
                    }
                }
            }
        }
        Ok(())
    });
    for item in &sel.from_clause {
        join_uses(item, &mut used);
    }
    used
}

/// A node kind whose children `walk_expr` visits (or a leaf). Any other
/// kind might hide a column reference, so it keeps every output.
fn walked_kind(n: &pg_query::protobuf::Node) -> bool {
    matches!(
        n.node.as_ref(),
        None | Some(
            N::ColumnRef(_)
                | N::AConst(_)
                | N::ParamRef(_)
                | N::SqlvalueFunction(_)
                | N::TypeCast(_)
                | N::AExpr(_)
                | N::FuncCall(_)
                | N::WindowDef(_)
                | N::SortBy(_)
                | N::ResTarget(_)
                | N::BooleanTest(_)
                | N::CollateClause(_)
                | N::NamedArgExpr(_)
                | N::GroupingSet(_)
                | N::List(_)
                | N::SubLink(_)
                | N::BoolExpr(_)
                | N::AArrayExpr(_)
                | N::RowExpr(_)
                | N::CoalesceExpr(_)
                | N::MinMaxExpr(_)
                | N::NullTest(_)
                | N::CaseExpr(_)
                | N::AIndirection(_)
        )
    )
}

/// A FROM item whose references the walk sees.
fn plain_from(item: &pg_query::protobuf::Node) -> bool {
    match item.node.as_ref() {
        Some(N::RangeVar(_) | N::RangeSubselect(_) | N::RangeFunction(_)) => true,
        Some(N::JoinExpr(j)) => [j.larg.as_deref(), j.rarg.as_deref()]
            .into_iter()
            .flatten()
            .all(plain_from),
        _ => false,
    }
}

fn join_uses(item: &pg_query::protobuf::Node, used: &mut Used) {
    if !plain_from(item) {
        used.all = true;
    }
    if let Some(N::JoinExpr(j)) = item.node.as_ref() {
        if j.is_natural || j.alias.as_ref().is_some_and(|a| !a.colnames.is_empty()) {
            used.all = true;
        }
        for u in &j.using_clause {
            if let Some(N::String(s)) = u.node.as_ref() {
                used.names.insert(s.sval.clone());
            }
        }
        for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
            join_uses(side, used);
        }
    }
}

/// Prune the `i`th FROM-subquery of `sel`. True when an output changed.
fn prune_one(sel: &mut pg_query::protobuf::SelectStmt, i: usize, ctx: &Ctx<'_>) -> bool {
    // Take the body out, so its own references do not count as readers.
    let (alias, colnames, mut body, lateral) = {
        let Some(rs) = nth_subquery(&mut sel.from_clause, i) else {
            return false;
        };
        let lateral = rs.lateral;
        let Some(alias) = rs.alias.as_ref() else {
            return false;
        };
        let colnames: Vec<String> = alias
            .colnames
            .iter()
            .filter_map(|c| match c.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect();
        let name = alias.aliasname.clone();
        let Some(body) = rs.subquery.take() else {
            return false;
        };
        (name, colnames, body, lateral)
    };
    let used = used_names(sel, &alias);
    let mut changed = false;
    if !used.all {
        if let Some(N::SelectStmt(inner)) = body.node.as_mut() {
            let names = output_names(inner, &colnames);
            if let Some(names) = names {
                let keep: Vec<bool> = names
                    .iter()
                    .map(|n| n.as_ref().is_none_or(|n| used.names.contains(n)))
                    .collect();
                changed = if lateral {
                    // A LATERAL body reads its left siblings, so it never
                    // plans alone: it is tried where it stands, in the
                    // FROM list it belongs to.
                    let mut pruned = (**inner).clone();
                    if prune_arms(&mut pruned, &keep) {
                        let mut from = sel.from_clause.clone();
                        if let Some(rs) = nth_subquery(&mut from, i) {
                            rs.subquery = Some(Box::new(pg_query::protobuf::Node {
                                node: Some(N::SelectStmt(inner.clone())),
                            }));
                        }
                        let trial = pg_query::protobuf::SelectStmt {
                            target_list: vec![one_target()],
                            from_clause: from,
                            op: pg_query::protobuf::SetOperation::SetopNone as i32,
                            limit_option: pg_query::protobuf::LimitOption::Default as i32,
                            ..Default::default()
                        };
                        if lateral_plans(&trial, ctx) {
                            **inner = pruned;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    prune_checked(inner, &keep, ctx)
                };
            }
        }
    }
    if let Some(rs) = nth_subquery(&mut sel.from_clause, i) {
        rs.subquery = Some(body);
    }
    changed
}

/// Each output's name as the enclosing query sees it (`None`: unknown, so it
/// is kept). `None` overall when the shape is not one PostgreSQL prunes.
fn output_names(
    inner: &pg_query::protobuf::SelectStmt,
    colnames: &[String],
) -> Option<Vec<Option<String>>> {
    let first = first_arm(inner)?;
    Some(
        first
            .target_list
            .iter()
            .enumerate()
            .map(|(k, t)| {
                if let Some(c) = colnames.get(k) {
                    return Some(c.clone());
                }
                let N::ResTarget(rt) = t.node.as_ref()? else {
                    return None;
                };
                if !rt.name.is_empty() {
                    return Some(rt.name.clone());
                }
                figure_colname(rt.val.as_deref()?)
            })
            .collect(),
    )
}

/// The leftmost arm of a UNION ALL tree (the select itself when it is not a
/// set operation); `None` for any other set operation.
fn first_arm(s: &pg_query::protobuf::SelectStmt) -> Option<&pg_query::protobuf::SelectStmt> {
    match (s.larg.as_deref(), s.rarg.as_deref()) {
        (None, None) => Some(s),
        (Some(l), Some(_)) if union_all(s) => first_arm(l),
        _ => None,
    }
}

fn union_all(s: &pg_query::protobuf::SelectStmt) -> bool {
    s.op == pg_query::protobuf::SetOperation::SetopUnion as i32
        && s.all
        && s.sort_clause.is_empty()
        && s.limit_count.is_none()
        && s.limit_offset.is_none()
        && s.with_clause.is_none()
}

/// `prune_arms`, applied only when something changes and the body plans
/// on its own (`plans_alone`).
fn prune_checked(body: &mut pg_query::protobuf::SelectStmt, keep: &[bool], ctx: &Ctx<'_>) -> bool {
    let mut pruned = body.clone();
    if !prune_arms(&mut pruned, keep) || !plans_alone(body, ctx) {
        return false;
    }
    *body = pruned;
    true
}

/// Prune the outputs `keep` says nothing reads, in every arm of a UNION ALL.
fn prune_arms(s: &mut pg_query::protobuf::SelectStmt, keep: &[bool]) -> bool {
    match (s.larg.is_some(), s.rarg.is_some()) {
        (false, false) => prune_select(s, keep),
        (true, true) if union_all(s) => {
            let mut changed = false;
            for side in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                changed |= prune_arms(side, keep);
            }
            changed
        }
        _ => false,
    }
}

fn prune_select(s: &mut pg_query::protobuf::SelectStmt, keep: &[bool]) -> bool {
    if !s.values_lists.is_empty() {
        return false;
    }
    // A plain DISTINCT (a list holding one empty node) reads every output.
    if s.distinct_clause.iter().any(|d| d.node.is_none()) {
        return false;
    }
    // Outputs the subquery's own clauses name, by name or position.
    let mut names: HashSet<String> = HashSet::new();
    let mut positions: HashSet<i64> = HashSet::new();
    let mut clauses: Vec<pg_query::protobuf::Node> = s
        .sort_clause
        .iter()
        .chain(s.group_clause.iter())
        .chain(s.distinct_clause.iter())
        .chain(s.window_clause.iter())
        .cloned()
        .collect();
    for c in &mut clauses {
        let _ = walk_expr(c, &mut |node| {
            match node.node.as_ref() {
                Some(N::ColumnRef(cr)) => {
                    for f in &cr.fields {
                        if let Some(N::String(st)) = f.node.as_ref() {
                            names.insert(st.sval.clone());
                        }
                    }
                }
                Some(N::AConst(a)) => {
                    if let Some(pg_query::protobuf::a_const::Val::Ival(i)) = a.val.as_ref() {
                        positions.insert(i64::from(i.ival));
                    }
                }
                _ => {}
            }
            Ok(())
        });
    }
    let aggregate_query =
        s.group_clause.is_empty() && (crate::has_aggregate(s) || s.having_clause.is_some());
    let mut changed = false;
    for k in 0..s.target_list.len() {
        if keep.get(k).copied().unwrap_or(true) || positions.contains(&(k as i64 + 1)) {
            continue;
        }
        let Some(N::ResTarget(rt)) = s.target_list[k].node.as_ref() else {
            continue;
        };
        if !rt.name.is_empty() && names.contains(&rt.name) {
            continue;
        }
        let Some(val) = rt.val.as_deref() else {
            continue;
        };
        if is_null(val) || is_count_star(val) || !removable(val) {
            continue;
        }
        let mut next = s.target_list[k].clone();
        if let Some(N::ResTarget(rt)) = next.node.as_mut() {
            rt.val = Some(Box::new(null_const()));
        }
        let previous = std::mem::replace(&mut s.target_list[k], next);
        if aggregate_query && !(crate::has_aggregate(s) || s.having_clause.is_some()) {
            // The last aggregate: `count(*)` keeps the query one row and
            // cannot raise.
            match count_star() {
                Some(c) => {
                    if let Some(N::ResTarget(rt)) = s.target_list[k].node.as_mut() {
                        rt.val = Some(Box::new(c));
                    }
                }
                None => {
                    s.target_list[k] = previous;
                    continue;
                }
            }
        }
        changed = true;
    }
    changed
}

/// `1`, as a select-list item.
fn one_target() -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            val: Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::AConst(pg_query::protobuf::AConst {
                    val: Some(pg_query::protobuf::a_const::Val::Ival(
                        pg_query::protobuf::Integer { ival: 1 },
                    )),
                    ..Default::default()
                })),
            })),
            location: -1,
            ..Default::default()
        }))),
    }
}

fn count_star() -> Option<pg_query::protobuf::Node> {
    let parsed = pg_query::parse("select count(*)").ok()?;
    match parsed
        .protobuf
        .stmts
        .first()?
        .stmt
        .as_ref()?
        .node
        .as_ref()?
    {
        N::SelectStmt(sel) => match sel.target_list.first()?.node.as_ref()? {
            N::ResTarget(rt) => rt.val.as_deref().cloned(),
            _ => None,
        },
        _ => None,
    }
}

fn is_count_star(n: &pg_query::protobuf::Node) -> bool {
    matches!(n.node.as_ref(), Some(N::FuncCall(f))
        if f.agg_star && f.over.is_none() && f.agg_filter.is_none()
            && crate::func_name(f).as_deref() == Some("count"))
}

fn is_null(n: &pg_query::protobuf::Node) -> bool {
    matches!(n.node.as_ref(), Some(N::AConst(a)) if a.isnull)
}

fn null_const() -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            isnull: true,
            location: -1,
            val: None,
        })),
    }
}

/// May this output be replaced by NULL without changing anything but
/// whether it raises? Not when it holds a call that might be volatile or
/// return a set, or a node this walk does not know.
fn removable(val: &pg_query::protobuf::Node) -> bool {
    let mut ok = true;
    let mut node = val.clone();
    let _ = walk_expr(&mut node, &mut |n| {
        ok &= node_removable(n);
        Ok(())
    });
    ok
}

fn node_removable(n: &pg_query::protobuf::Node) -> bool {
    match n.node.as_ref() {
        None => true,
        // A string literal is coerced to its operator's type during
        // PostgreSQL's parse analysis (`'a' + 1` is 22P02 before anything is
        // removed); this planner coerces it when the row is computed, so an
        // output holding one is kept and computed.
        Some(N::AConst(a)) => !matches!(a.val, Some(pg_query::protobuf::a_const::Val::Sval(_))),
        Some(
            N::ColumnRef(_)
            | N::AExpr(_)
            | N::TypeCast(_)
            | N::BoolExpr(_)
            | N::CaseExpr(_)
            | N::CaseWhen(_)
            | N::NullTest(_)
            | N::BooleanTest(_)
            | N::CoalesceExpr(_)
            | N::MinMaxExpr(_)
            | N::AArrayExpr(_)
            | N::RowExpr(_)
            | N::CollateClause(_)
            | N::ResTarget(_)
            | N::List(_)
            | N::SortBy(_)
            | N::WindowDef(_)
            | N::NamedArgExpr(_)
            | N::AIndirection(_)
            | N::AIndices(_)
            | N::ParamRef(_)
            | N::SqlvalueFunction(_),
        ) => true,
        Some(N::FuncCall(f)) => {
            let Some(name) = crate::func_name(f) else {
                return false;
            };
            if crate::correlated::user_function_named(&name) || returns_set(&name) {
                return false;
            }
            f.over.is_some()
                || crate::aggregate_func(&name, f.agg_within_group).is_some()
                || crate::correlated::immutable_builtin(&name, f.args.len())
        }
        Some(N::SubLink(sl)) => match sl.subselect.as_deref().and_then(|q| q.node.as_ref()) {
            Some(N::SelectStmt(body)) => sublink_removable(body),
            _ => false,
        },
        _ => false,
    }
}

/// A subquery inside an output: removable when it reads plain tables only
/// and every expression in it is.
fn sublink_removable(body: &pg_query::protobuf::SelectStmt) -> bool {
    if body.larg.is_some() || body.with_clause.is_some() || !body.locking_clause.is_empty() {
        return false;
    }
    if !body
        .from_clause
        .iter()
        .all(|f| matches!(f.node.as_ref(), Some(N::RangeVar(_))))
    {
        return false;
    }
    let mut ok = true;
    let mut copy = body.clone();
    let _ = crate::correlated::walk_select(&mut copy, &mut |n| {
        ok &= node_removable(n);
        Ok(())
    });
    ok
}

/// Does any PostgreSQL 15 built-in of this name return a set?
fn returns_set(name: &str) -> bool {
    static SET: std::sync::OnceLock<HashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        include_str!("pg_proc_sigs.tsv")
            .lines()
            .filter_map(|l| {
                let cols: Vec<&str> = l.split('\t').collect();
                (cols.get(5).map(|c| c.trim()) == Some("1")).then_some(cols[0])
            })
            .collect()
    })
    .contains(name)
}

/// PostgreSQL's `FigureColname`, for the shapes whose name is certain.
fn figure_colname(n: &pg_query::protobuf::Node) -> Option<String> {
    match n.node.as_ref()? {
        N::ColumnRef(c) => match c.fields.last()?.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        },
        N::FuncCall(f) => match f.funcname.last()?.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        },
        N::TypeCast(tc) => {
            // A cast of an unnamed expression takes the TYPE's name (the
            // grammar has already made `int` `int4`).
            match figure_colname(tc.arg.as_deref()?) {
                Some(inner) if inner != "?column?" => Some(inner),
                _ => match tc.type_name.as_ref()?.names.last()?.node.as_ref()? {
                    N::String(s) => Some(s.sval.clone()),
                    _ => None,
                },
            }
        }
        N::AExpr(_) | N::AConst(_) | N::BoolExpr(_) | N::NullTest(_) | N::BooleanTest(_) => {
            Some("?column?".into())
        }
        N::CaseExpr(_) => Some("case".into()),
        N::SubLink(sl) => {
            use pg_query::protobuf::SubLinkType as T;
            match T::try_from(sl.sub_link_type).ok()? {
                T::ExistsSublink => Some("exists".into()),
                T::ArraySublink => Some("array".into()),
                T::ExprSublink => match sl.subselect.as_deref()?.node.as_ref()? {
                    N::SelectStmt(b) if b.larg.is_none() => {
                        match b.target_list.first()?.node.as_ref()? {
                            N::ResTarget(rt) if !rt.name.is_empty() => Some(rt.name.clone()),
                            N::ResTarget(rt) => figure_colname(rt.val.as_deref()?),
                            _ => None,
                        }
                    }
                    _ => None,
                },
                _ => Some("?column?".into()),
            }
        }
        N::CoalesceExpr(_) => Some("coalesce".into()),
        N::AArrayExpr(_) => Some("array".into()),
        N::RowExpr(_) => Some("row".into()),
        _ => None,
    }
}
