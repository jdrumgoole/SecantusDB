//! Subqueries in a GROUPED query's select list, HAVING or ORDER BY.
//!
//! `SELECT d.name, (SELECT count(*) FROM e WHERE e.dept = d.name) FROM d
//! GROUP BY d.name HAVING count(*) > (SELECT ...)` needs the grouped rows to
//! exist before the subqueries run, and the aggregate planner has no per-row
//! stage of its own. So the query is split, before its subqueries are
//! resolved:
//!
//! ```sql
//! SELECT __g1 AS name, (SELECT count(*) FROM e WHERE e.dept = __g1)
//! FROM (SELECT d.name AS __g1, count(*) AS __a1 FROM d GROUP BY d.name) __grp
//! WHERE __a1 > (SELECT ...)
//! ```
//!
//! The inner query is an ordinary aggregate; the outer one is an ordinary
//! SELECT over a FROM subquery, where correlated subqueries already work.
//! Every aggregate call and every grouped column outside an aggregate
//! becomes a slot. Inside a subquery, an aggregate whose arguments read only
//! OUTER columns (`min(d.id)` in `WHERE x.dept_id = min(d.id)`) is the outer
//! query's aggregate, as PostgreSQL has it; `count(*)` there stays the
//! subquery's own.

use super::*;

struct Slots {
    /// `(print of the grouped expression, its bare column name if any, slot)`.
    groups: Vec<(String, Option<String>, String)>,
    /// `(print of the aggregate call, slot)`.
    aggs: Vec<(String, String)>,
    inner_targets: Vec<pg_query::protobuf::Node>,
    /// An outer column a subquery reads that is neither grouped nor inside
    /// an aggregate: PostgreSQL's 42803.
    ungrouped: Option<String>,
}

/// A slot as the outer query (or any subquery, at any depth) reads it:
/// qualified by the FROM subquery's alias, so a nested subquery finds it as
/// an outer reference the same way it finds `d.id`.
fn slot_ref(slot: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
            fields: vec![string_node("__grp"), string_node(slot)],
            location: -1,
        })),
    }
}

fn target(name: &str, val: pg_query::protobuf::Node) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            name: name.to_string(),
            val: Some(Box::new(val)),
            location: -1,
            ..Default::default()
        }))),
    }
}

/// Is this ORDER BY item a bare name one of the output columns carries?
fn bare_output_name(item: &pg_query::protobuf::Node, out_names: &[String]) -> bool {
    let Some(N::SortBy(sb)) = item.node.as_ref() else {
        return false;
    };
    match sb.node.as_deref().and_then(|n| n.node.as_ref()) {
        Some(N::ColumnRef(c)) if c.fields.len() == 1 => {
            column_ref_name(c).is_some_and(|n| out_names.contains(&n))
        }
        _ => false,
    }
}

fn contains_sublink(n: &pg_query::protobuf::Node) -> bool {
    let mut found = false;
    let mut n = n.clone();
    let _ = walk_expr(&mut n, &mut |x| {
        found |= matches!(x.node.as_ref(), Some(N::SubLink(_)));
        Ok(())
    });
    found
}

/// Names a subquery's own FROM exposes (tables and aliases).
fn from_names(s: &pg_query::protobuf::SelectStmt) -> Vec<String> {
    let mut out = Vec::new();
    for item in &s.from_clause {
        collect_from_names(item, &mut out);
    }
    out
}

/// Does `f`'s argument list read only columns qualified by something the
/// enclosing subquery does not define (so the aggregate is the OUTER one)?
fn reads_only_outer(f: &pg_query::protobuf::FuncCall, inner_names: &[String]) -> bool {
    let mut any = false;
    let mut all_outer = true;
    for a in &f.args {
        let mut a = a.clone();
        let _ = walk_expr(&mut a, &mut |x| {
            if let Some(N::ColumnRef(c)) = x.node.as_ref() {
                any = true;
                let parts: Vec<String> = c
                    .fields
                    .iter()
                    .filter_map(|p| match p.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.clone()),
                        _ => None,
                    })
                    .collect();
                let outer = parts.len() >= 2 && !inner_names.contains(&parts[parts.len() - 2]);
                all_outer &= outer;
            }
            Ok(())
        });
    }
    any && all_outer
}

impl Slots {
    fn agg_slot(&mut self, call: &pg_query::protobuf::Node) -> String {
        let print = node_print(call);
        if let Some((_, s)) = self.aggs.iter().find(|(p, _)| *p == print) {
            return s.clone();
        }
        let slot = format!("__a{}", self.aggs.len() + 1);
        self.inner_targets.push(target(&slot, call.clone()));
        self.aggs.push((print, slot.clone()));
        slot
    }

    fn group_slot(&self, n: &pg_query::protobuf::Node) -> Option<String> {
        let print = node_print(n);
        if let Some((_, _, s)) = self.groups.iter().find(|(p, _, _)| *p == print) {
            return Some(s.clone());
        }
        if let Some(N::ColumnRef(c)) = n.node.as_ref() {
            let name = column_ref_name(c)?;
            return self
                .groups
                .iter()
                .find(|(_, col, _)| col.as_deref() == Some(name.as_str()))
                .map(|(_, _, s)| s.clone());
        }
        None
    }

    /// Rewrite an outer expression: aggregates and grouped columns to slots.
    /// `inner` is `Some(names)` inside a subquery (whose own aggregates and
    /// columns stay put unless they read only the outer query).
    fn rewrite(&mut self, n: &mut pg_query::protobuf::Node, inner: Option<&[String]>) {
        // A grouped EXPRESSION (`GROUP BY n % 2`) written again in the
        // outer query is its slot, wherever it sits (`(n % 2)::text`).
        if inner.is_none()
            && !matches!(
                n.node.as_ref(),
                Some(N::ColumnRef(_)) | Some(N::AConst(_)) | None
            )
        {
            if let Some(slot) = self.group_slot(n) {
                *n = slot_ref(&slot);
                return;
            }
        }
        match n.node.as_mut() {
            Some(N::FuncCall(f)) if is_aggregate_call(f) && f.over.is_none() => {
                let hoist = match inner {
                    None => true,
                    Some(names) => reads_only_outer(f, names),
                };
                if hoist {
                    let slot = self.agg_slot(n);
                    *n = slot_ref(&slot);
                    return;
                }
            }
            Some(N::ColumnRef(c)) => {
                let c = c.clone();
                let qualified_outer = inner.is_some_and(|names| {
                    c.fields.len() >= 2
                        && matches!(c.fields[c.fields.len() - 2].node.as_ref(),
                            Some(N::String(q)) if !names.contains(&q.sval))
                });
                if inner.is_none() || qualified_outer {
                    match self.group_slot(n) {
                        Some(slot) => *n = slot_ref(&slot),
                        None if qualified_outer && self.ungrouped.is_none() => {
                            let parts: Vec<String> = c
                                .fields
                                .iter()
                                .filter_map(|p| match p.node.as_ref() {
                                    Some(N::String(s)) => Some(s.sval.clone()),
                                    _ => None,
                                })
                                .collect();
                            self.ungrouped = Some(parts.join("."));
                        }
                        None => {}
                    }
                }
                return;
            }
            Some(N::SubLink(sl)) => {
                if let Some(N::SelectStmt(body)) =
                    sl.subselect.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    let names = from_names(body);
                    self.rewrite_select(body, &names);
                }
                if let Some(t) = sl.testexpr.as_deref_mut() {
                    self.rewrite(t, inner);
                }
                return;
            }
            _ => {}
        }
        // Descend one level into the node's children.
        let mut children: Vec<&mut pg_query::protobuf::Node> = Vec::new();
        match n.node.as_mut() {
            Some(N::AExpr(e)) => {
                children.extend(e.lexpr.as_deref_mut());
                children.extend(e.rexpr.as_deref_mut());
            }
            Some(N::BoolExpr(b)) => children.extend(b.args.iter_mut()),
            Some(N::FuncCall(f)) => children.extend(f.args.iter_mut()),
            Some(N::TypeCast(t)) => children.extend(t.arg.as_deref_mut()),
            Some(N::NullTest(t)) => children.extend(t.arg.as_deref_mut()),
            Some(N::BooleanTest(t)) => children.extend(t.arg.as_deref_mut()),
            Some(N::CoalesceExpr(c)) => children.extend(c.args.iter_mut()),
            Some(N::MinMaxExpr(m)) => children.extend(m.args.iter_mut()),
            Some(N::List(l)) => children.extend(l.items.iter_mut()),
            Some(N::RowExpr(r)) => children.extend(r.args.iter_mut()),
            Some(N::AArrayExpr(a)) => children.extend(a.elements.iter_mut()),
            // `(array_agg(x))[1]`: the subscripted value and its subscripts.
            Some(N::AIndirection(a)) => {
                children.extend(a.arg.as_deref_mut());
                for i in &mut a.indirection {
                    if let Some(N::AIndices(ix)) = i.node.as_mut() {
                        children.extend(ix.lidx.as_deref_mut());
                        children.extend(ix.uidx.as_deref_mut());
                    }
                }
            }
            Some(N::CaseExpr(c)) => {
                children.extend(c.arg.as_deref_mut());
                for w in &mut c.args {
                    if let Some(N::CaseWhen(cw)) = w.node.as_mut() {
                        children.extend(cw.expr.as_deref_mut());
                        children.extend(cw.result.as_deref_mut());
                    }
                }
                children.extend(c.defresult.as_deref_mut());
            }
            Some(N::ResTarget(rt)) => children.extend(rt.val.as_deref_mut()),
            Some(N::SortBy(sb)) => children.extend(sb.node.as_deref_mut()),
            _ => {}
        }
        for c in children {
            self.rewrite(c, inner);
        }
    }

    fn rewrite_select(&mut self, s: &mut pg_query::protobuf::SelectStmt, names: &[String]) {
        for t in &mut s.target_list {
            self.rewrite(t, Some(names));
        }
        if let Some(w) = s.where_clause.as_deref_mut() {
            self.rewrite(w, Some(names));
        }
        if let Some(h) = s.having_clause.as_deref_mut() {
            self.rewrite(h, Some(names));
        }
    }
}

/// Split a grouped SELECT whose select list, HAVING or ORDER BY holds a
/// subquery; `None` when there is nothing to split (or a shape this does not
/// split: DISTINCT, windows, grouping sets, set operations).
pub(crate) fn split(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let Some(out) = split_inner(s) else {
        return Ok(None);
    };
    match out {
        (_, Some(col)) => Err(Error::Grouping(format!(
            "subquery uses ungrouped column \"{col}\" from outer query"
        ))),
        (stmt, None) => Ok(Some(stmt)),
    }
}

/// Split a grouped SELECT whose select list, HAVING or ORDER BY computes
/// over its groups -- `n::text ... GROUP BY n`, `-n, count(*)` -- which the
/// aggregate planner refuses: the inner query groups, the outer one computes.
/// The planner's fallback when the grouped plan is unsupported. An outer
/// column that is neither grouped nor aggregated is PostgreSQL's 42803.
pub(crate) fn split_expressions(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let Some((stmt, ungrouped)) = split_inner_with(s, true) else {
        return Ok(None);
    };
    if let Some(col) = ungrouped {
        return Err(Error::Grouping(format!(
            "subquery uses ungrouped column \"{col}\" from outer query"
        )));
    }
    let mut stray: Option<Vec<String>> = None;
    let mut check = |n: &pg_query::protobuf::Node| {
        let mut n = n.clone();
        let _ = walk_expr(&mut n, &mut |x| {
            match x.node.as_ref() {
                // A subquery's own columns are its business; blank it so
                // the walk does not descend.
                Some(N::SubLink(_)) => *x = pg_query::protobuf::Node { node: None },
                Some(N::ColumnRef(c)) => {
                    let parts: Vec<String> = c
                        .fields
                        .iter()
                        .filter_map(|p| match p.node.as_ref() {
                            Some(N::String(s)) => Some(s.sval.clone()),
                            _ => None,
                        })
                        .collect();
                    if parts.first().map(String::as_str) != Some("__grp") && stray.is_none() {
                        stray = Some(parts);
                    }
                }
                _ => {}
            }
            Ok(())
        });
    };
    for t in &stmt.target_list {
        check(t);
    }
    if let Some(w) = stmt.where_clause.as_deref() {
        check(w);
    }
    let out_names: Vec<String> = stmt
        .target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => Some(rt.name.clone()),
            _ => None,
        })
        .collect();
    for o in &stmt.sort_clause {
        if !bare_output_name(o, &out_names) {
            check(o);
        }
    }
    if let Some(mut parts) = stray {
        if parts.len() == 1 {
            if let [only] = s.from_clause.as_slice() {
                if let Some(N::RangeVar(rv)) = only.node.as_ref() {
                    let q = rv
                        .alias
                        .as_ref()
                        .map(|a| a.aliasname.clone())
                        .unwrap_or_else(|| rv.relname.clone());
                    parts.insert(0, q);
                }
            }
        }
        return Err(Error::Grouping(format!(
            "column \"{}\" must appear in the GROUP BY clause or be used in an aggregate function",
            parts.join(".")
        )));
    }
    Ok(Some(stmt))
}

fn split_inner(
    s: &pg_query::protobuf::SelectStmt,
) -> Option<(pg_query::protobuf::SelectStmt, Option<String>)> {
    split_inner_with(s, false)
}

fn split_inner_with(
    s: &pg_query::protobuf::SelectStmt,
    force: bool,
) -> Option<(pg_query::protobuf::SelectStmt, Option<String>)> {
    let grouped = !s.group_clause.is_empty() || has_aggregate(s);
    if !grouped
        || !s.distinct_clause.is_empty()
        || !s.window_clause.is_empty()
        || has_window(s)
        || s.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || s.group_clause
            .iter()
            .any(|g| matches!(g.node.as_ref(), Some(N::GroupingSet(_))))
    {
        return None;
    }
    let has = s.target_list.iter().any(contains_sublink)
        || s.having_clause.as_deref().is_some_and(contains_sublink)
        || s.sort_clause.iter().any(contains_sublink);
    if !has && !force {
        return None;
    }
    let mut slots = Slots {
        groups: Vec::new(),
        aggs: Vec::new(),
        inner_targets: Vec::new(),
        ungrouped: None,
    };
    for (i, g) in s.group_clause.iter().enumerate() {
        let slot = format!("__g{}", i + 1);
        let col = match g.node.as_ref() {
            Some(N::ColumnRef(c)) => column_ref_name(c),
            _ => None,
        };
        slots.groups.push((node_print(g), col, slot.clone()));
        slots.inner_targets.push(target(&slot, g.clone()));
    }
    let mut outer_targets = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return None;
        };
        let mut val = rt.val.as_deref()?.clone();
        let name = if rt.name.is_empty() {
            match val.node.as_ref() {
                Some(N::SubLink(sl))
                    if SubLinkType::try_from(sl.sub_link_type)
                        == Ok(SubLinkType::ExistsSublink) =>
                {
                    "exists".to_string()
                }
                Some(N::SubLink(_)) => {
                    // A scalar subquery is named after its own single column.
                    match &val.node {
                        Some(N::SubLink(sl)) => {
                            match sl.subselect.as_deref().and_then(|q| q.node.as_ref()) {
                                Some(N::SelectStmt(b)) => {
                                    match b.target_list.first().and_then(|t| t.node.as_ref()) {
                                        Some(N::ResTarget(r)) if !r.name.is_empty() => {
                                            r.name.clone()
                                        }
                                        Some(N::ResTarget(r)) => r
                                            .val
                                            .as_deref()
                                            .map(expression_column_name)
                                            .unwrap_or_else(|| "?column?".into()),
                                        _ => "?column?".into(),
                                    }
                                }
                                _ => "?column?".into(),
                            }
                        }
                        _ => "?column?".into(),
                    }
                }
                _ => expression_column_name(&val),
            }
        } else {
            rt.name.clone()
        };
        slots.rewrite(&mut val, None);
        outer_targets.push(target(&name, val));
    }
    let mut having = s.having_clause.as_deref().cloned();
    if let Some(h) = having.as_mut() {
        slots.rewrite(h, None);
    }
    // A bare name in ORDER BY that is an OUTPUT column's name is that output
    // column, before it is any input column (`SELECT n::text ... ORDER BY n`
    // sorts the text): the outer query resolves it, so it stays as written.
    let out_names: Vec<String> = outer_targets
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) => Some(rt.name.clone()),
            _ => None,
        })
        .collect();
    let mut sort = s.sort_clause.clone();
    for item in &mut sort {
        if bare_output_name(item, &out_names) {
            continue;
        }
        slots.rewrite(item, None);
    }
    // An output alias in ORDER BY names the output column, which the outer
    // query still has under the same name.
    let mut inner = s.clone();
    inner.target_list = slots.inner_targets.clone();
    inner.having_clause = None;
    inner.sort_clause = Vec::new();
    inner.limit_count = None;
    inner.limit_offset = None;
    inner.limit_option = pg_query::protobuf::LimitOption::Default as i32;
    if inner.target_list.is_empty() {
        inner.target_list.push(target(
            "__n",
            pg_query::protobuf::Node {
                node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
                    funcname: vec![string_node("count")],
                    agg_star: true,
                    funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
                    location: -1,
                    ..Default::default()
                }))),
            },
        ));
    }
    let ungrouped = slots.ungrouped.take();
    Some((
        pg_query::protobuf::SelectStmt {
            target_list: outer_targets,
            from_clause: vec![pg_query::protobuf::Node {
                node: Some(N::RangeSubselect(Box::new(
                    pg_query::protobuf::RangeSubselect {
                        lateral: false,
                        subquery: Some(Box::new(pg_query::protobuf::Node {
                            node: Some(N::SelectStmt(Box::new(inner))),
                        })),
                        alias: Some(pg_query::protobuf::Alias {
                            aliasname: "__grp".into(),
                            colnames: Vec::new(),
                        }),
                    },
                ))),
            }],
            where_clause: having.map(Box::new),
            sort_clause: sort,
            limit_count: s.limit_count.clone(),
            limit_offset: s.limit_offset.clone(),
            limit_option: s.limit_option,
            op: pg_query::protobuf::SetOperation::SetopNone as i32,
            ..Default::default()
        },
        ungrouped,
    ))
}
