//! PostgreSQL's pull-up of a one-row constant FROM item, as far as it is
//! visible to a client: the CONTEXT of an error raised by an inlined
//! `LANGUAGE sql` function.
//!
//! PostgreSQL pulls a FROM-subquery that is ONE row of constants --
//! `(values (0)) v(a)`, `(select 0) v(a)`, `(select 0 as a) v` -- up into
//! its parent, so a reference to its column becomes the constant. A user
//! function called over such a column is then a call over constants, which
//! the planner inlines and folds: its error says `SQL function "f" during
//! inlining`, and it is raised even when no row would reach the call.
//!
//! Here the column stays a per-row value, so [`rewrite`] substitutes the
//! constant for the column inside a user function call's ARGUMENTS only --
//! nowhere else, so output column names and every other expression are
//! untouched -- and the call is classified (and folded) as PostgreSQL's is.

use super::*;

/// Rewrite every `SELECT` in `node` (its FROM-subqueries included).
pub(crate) fn rewrite(node: &mut pg_query::protobuf::Node) {
    if let Some(N::SelectStmt(sel)) = node.node.as_mut() {
        rewrite_select(sel);
    }
}

fn rewrite_select(sel: &mut pg_query::protobuf::SelectStmt) {
    if let Some(l) = sel.larg.as_deref_mut() {
        rewrite_select(l);
    }
    if let Some(r) = sel.rarg.as_deref_mut() {
        rewrite_select(r);
    }
    // (qualifier, column) -> constant, for every one-row constant FROM item.
    let mut map: Vec<(String, String, pg_query::protobuf::Node)> = Vec::new();
    let mut all_pulled = !sel.from_clause.is_empty();
    for item in &mut sel.from_clause {
        let Some(N::RangeSubselect(rs)) = item.node.as_mut() else {
            all_pulled = false;
            continue;
        };
        let Some(sub) = rs.subquery.as_deref_mut() else {
            all_pulled = false;
            continue;
        };
        if let Some(N::SelectStmt(inner)) = sub.node.as_mut() {
            rewrite_select(inner);
        }
        let pulled = match (rs.lateral, rs.alias.as_ref(), sub.node.as_ref()) {
            (false, Some(alias), Some(N::SelectStmt(inner))) => {
                one_row_constants(inner).map(|cols| (alias, cols))
            }
            _ => None,
        };
        let Some((alias, cols)) = pulled else {
            all_pulled = false;
            continue;
        };
        let names: Vec<String> = alias
            .colnames
            .iter()
            .filter_map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect();
        for (i, (name, value)) in cols.into_iter().enumerate() {
            let Some(name) = names.get(i).cloned().or(name) else {
                continue;
            };
            map.push((alias.aliasname.clone(), name, value));
        }
    }
    if map.is_empty() {
        return;
    }
    // A bare name is substituted only when every FROM item was pulled up
    // (a table could have a column of that name too) and exactly one of
    // them has it.
    let lookup = |c: &pg_query::protobuf::ColumnRef| -> Option<pg_query::protobuf::Node> {
        let parts: Vec<&str> = c
            .fields
            .iter()
            .map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        match parts.as_slice() {
            [q, col] => map
                .iter()
                .find(|(a, n, _)| a == q && n == col)
                .map(|(_, _, v)| v.clone()),
            [col] if all_pulled => {
                let mut hits = map.iter().filter(|(_, n, _)| n == col);
                let first = hits.next()?;
                hits.next().is_none().then(|| first.2.clone())
            }
            _ => None,
        }
    };
    let mut visit = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        let Some(N::FuncCall(f)) = n.node.as_mut() else {
            return Ok(());
        };
        let user = func_name(f).is_some_and(|name| {
            correlated::user_function_for(&name, &f.args).is_some_and(|u| !u.returns_set)
        });
        if !user || f.over.is_some() {
            return Ok(());
        }
        for arg in &mut f.args {
            walk_expr(arg, &mut |a| {
                if let Some(N::ColumnRef(c)) = a.node.as_ref() {
                    if let Some(v) = lookup(c) {
                        *a = v;
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    };
    for t in &mut sel.target_list {
        let _ = walk_expr(t, &mut visit);
    }
    if let Some(w) = sel.where_clause.as_deref_mut() {
        let _ = walk_expr(w, &mut visit);
    }
}

/// The columns of a one-row constant subquery, each with its name when it
/// carries one (a VALUES list's do not), or `None` when it is not one.
fn one_row_constants(
    s: &pg_query::protobuf::SelectStmt,
) -> Option<Vec<(Option<String>, pg_query::protobuf::Node)>> {
    if s.larg.is_some()
        || s.with_clause.is_some()
        || s.where_clause.is_some()
        || !s.group_clause.is_empty()
        || s.having_clause.is_some()
        || !s.sort_clause.is_empty()
        || s.limit_count.is_some()
        || s.limit_offset.is_some()
        || !s.distinct_clause.is_empty()
        || !s.locking_clause.is_empty()
        || !s.window_clause.is_empty()
    {
        return None;
    }
    let row: Vec<(Option<String>, &pg_query::protobuf::Node)> = if !s.values_lists.is_empty() {
        let [row] = s.values_lists.as_slice() else {
            return None;
        };
        let Some(N::List(l)) = row.node.as_ref() else {
            return None;
        };
        l.items.iter().map(|e| (None, e)).collect()
    } else {
        if !s.from_clause.is_empty() || s.target_list.is_empty() {
            return None;
        }
        s.target_list
            .iter()
            .map(|t| match t.node.as_ref() {
                Some(N::ResTarget(rt)) => rt
                    .val
                    .as_deref()
                    .map(|v| ((!rt.name.is_empty()).then(|| rt.name.clone()), v)),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?
    };
    row.into_iter()
        .map(|(name, e)| constant(e).map(|v| (name, v)))
        .collect()
}

/// `e` as the constant PostgreSQL pulls up -- a literal, a cast of one, or
/// an operator over them; an untyped string literal resolves to `text` in
/// a subquery's output, so it is cast so.
fn constant(e: &pg_query::protobuf::Node) -> Option<pg_query::protobuf::Node> {
    fn plain(e: &pg_query::protobuf::Node) -> bool {
        match e.node.as_ref() {
            Some(N::AConst(c)) => !c.isnull,
            Some(N::TypeCast(tc)) => tc.arg.as_deref().is_some_and(plain),
            Some(N::AExpr(a)) => {
                a.kind == pg_query::protobuf::AExprKind::AexprOp as i32
                    && a.lexpr.as_deref().is_none_or(plain)
                    && a.rexpr.as_deref().is_some_and(plain)
            }
            _ => false,
        }
    }
    if !plain(e) {
        return None;
    }
    let string = matches!(
        e.node.as_ref(),
        Some(N::AConst(c)) if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
    );
    if !string {
        return Some(e.clone());
    }
    Some(pg_query::protobuf::Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(e.clone())),
            type_name: Some(pg_query::protobuf::TypeName {
                names: vec![pg_query::protobuf::Node {
                    node: Some(N::String(pg_query::protobuf::String {
                        sval: "text".into(),
                    })),
                }],
                typemod: -1,
                ..Default::default()
            }),
            location: -1,
        }))),
    })
}
