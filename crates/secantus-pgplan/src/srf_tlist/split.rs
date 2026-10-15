//! The split a grouped or windowed select needs before its set-returning
//! targets are lifted: PostgreSQL's ProjectSet runs ABOVE the Agg and the
//! WindowAgg (`select count(*), generate_series(1, 3) from t` is the count
//! three times, measured on 15.19), so the grouping becomes a subquery and
//! the set-returning targets run over its output rows.

use super::*;

/// Whether the expression reads anything of the row it is evaluated over: a
/// column, a parameter, a subquery, an aggregate or a window.
fn reads_row(n: &Node) -> bool {
    let mut probe = n.clone();
    let mut hit = false;
    let _ = walk_expr(&mut probe, &mut |m| {
        hit |= match m.node.as_ref() {
            Some(N::ColumnRef(_) | N::ParamRef(_) | N::SubLink(_) | N::GroupingFunc(_)) => true,
            Some(N::FuncCall(f)) => {
                f.over.is_some()
                    || f.agg_star
                    || func_name(f).is_some_and(|name| {
                        aggregate_func(&name, f.agg_within_group).is_some()
                            || user_agg::is_user_aggregate(&name)
                    })
            }
            _ => false,
        };
        Ok(())
    });
    hit
}

/// A grouped or windowed select whose ProjectSet targets must run over its
/// OUTPUT rows: the grouping as a subquery `__agg` producing every
/// set-returning-free piece the targets, ORDER BY and DISTINCT read, and the
/// set-returning targets over it. `None` when no call is left above the
/// grouping (every one is a GROUP BY expression).
pub(super) fn split_above_grouping(s: &SelectStmt) -> Result<Option<SelectStmt>> {
    let output_expr = |g: &Node| -> Option<Node> {
        let val = |rt: &pg_query::protobuf::ResTarget| rt.val.as_deref().cloned();
        match g.node.as_ref() {
            Some(N::AConst(c)) => match &c.val {
                Some(a_const::Val::Ival(i)) => {
                    let k = usize::try_from(i.ival).ok()?.checked_sub(1)?;
                    match s.target_list.get(k)?.node.as_ref() {
                        Some(N::ResTarget(rt)) => val(rt),
                        _ => None,
                    }
                }
                _ => None,
            },
            // An output name stands for its expression when that expression
            // is set-returning (no input column is named like one).
            Some(N::ColumnRef(c)) => {
                let name = match c.fields.as_slice() {
                    [Node {
                        node: Some(N::String(sv)),
                    }] => sv.sval.clone(),
                    _ => return None,
                };
                s.target_list.iter().find_map(|t| match t.node.as_ref() {
                    Some(N::ResTarget(rt)) if rt.name == name => val(rt).filter(contains_srf),
                    _ => None,
                })
            }
            _ => None,
        }
    };
    let group: Vec<Node> = s
        .group_clause
        .iter()
        .map(|g| output_expr(g).unwrap_or_else(|| g.clone()))
        .collect();
    let group_keys: Vec<String> = group.iter().filter_map(|g| deparse_expr(g).ok()).collect();
    let mut lifted: Vec<(String, Node)> = Vec::new();
    let mut lift = |n: &mut Node| -> Result<()> {
        walk_expr(n, &mut |m| {
            let Ok(key) = deparse_expr(m) else {
                return Ok(());
            };
            let grouped = group_keys.contains(&key);
            if !(grouped || (!contains_srf(m) && reads_row(m))) {
                return Ok(());
            }
            let k = match lifted.iter().position(|(k, _)| *k == key) {
                Some(k) => k,
                None => {
                    lifted.push((key, m.clone()));
                    lifted.len() - 1
                }
            };
            *m = column("__agg", &format!("__p{k}"));
            Ok(())
        })
    };
    let mut outer = SelectStmt {
        limit_option: s.limit_option,
        limit_count: s.limit_count.clone(),
        limit_offset: s.limit_offset.clone(),
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        ..Default::default()
    };
    let mut names = Vec::new();
    for t in &s.target_list {
        let mut t = t.clone();
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                if rt.name.is_empty() {
                    rt.name = expression_column_name(v);
                }
                lift(v)?;
            }
            names.push(rt.name.clone());
        }
        outer.target_list.push(t);
    }
    // An ORDER BY / DISTINCT ON position or output name stays the output's.
    let is_output = |n: &Node| match n.node.as_ref() {
        Some(N::AConst(c)) => matches!(c.val, Some(a_const::Val::Ival(_))),
        Some(N::ColumnRef(c)) => matches!(
            c.fields.as_slice(),
            [Node { node: Some(N::String(sv)) }] if names.contains(&sv.sval)
        ),
        _ => false,
    };
    for item in &s.sort_clause {
        let mut item = item.clone();
        if let Some(N::SortBy(sb)) = item.node.as_mut() {
            if let Some(v) = sb.node.as_deref_mut() {
                if !is_output(v) {
                    lift(v)?;
                }
            }
        }
        outer.sort_clause.push(item);
    }
    for item in &s.distinct_clause {
        let mut item = item.clone();
        if item.node.is_some() && !is_output(&item) {
            lift(&mut item)?;
        }
        outer.distinct_clause.push(item);
    }
    if !has_srf_site(&outer) {
        return Ok(None);
    }
    // A call INSIDE an aggregate or window (`sum(generate_series(1, 3))`,
    // PostgreSQL's 0A000) keeps that aggregate above the split, so splitting
    // again would never end; leave it to the per-level path.
    let mut still_grouped = false;
    for_each_site(&mut outer.clone(), &mut |n| {
        still_grouped |= node_has_window(Some(n)) || contains_nested_aggregate(n) || {
            matches!(n.node.as_ref(), Some(N::FuncCall(f))
                if f.over.is_none() && func_name(f).is_some_and(|name|
                    aggregate_func(&name, f.agg_within_group).is_some()
                        || user_agg::is_user_aggregate(&name)))
        };
        Ok(())
    })?;
    if still_grouped {
        return Ok(None);
    }
    if lifted.is_empty() {
        lifted.push((String::new(), parse_expression("1")?));
    }
    let inner = SelectStmt {
        target_list: lifted
            .into_iter()
            .enumerate()
            .map(|(k, (_, val))| Node {
                node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                    name: format!("__p{k}"),
                    val: Some(Box::new(val)),
                    location: -1,
                    ..Default::default()
                }))),
            })
            .collect(),
        from_clause: s.from_clause.clone(),
        where_clause: s.where_clause.clone(),
        group_clause: group,
        group_distinct: s.group_distinct,
        having_clause: s.having_clause.clone(),
        window_clause: s.window_clause.clone(),
        with_clause: s.with_clause.clone(),
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        ..Default::default()
    };
    outer.from_clause.push(Node {
        node: Some(N::RangeSubselect(Box::new(
            pg_query::protobuf::RangeSubselect {
                lateral: false,
                subquery: Some(Box::new(Node {
                    node: Some(N::SelectStmt(Box::new(inner))),
                })),
                alias: Some(pg_query::protobuf::Alias {
                    aliasname: "__agg".into(),
                    colnames: Vec::new(),
                }),
            },
        ))),
    });
    Ok(Some(outer))
}
