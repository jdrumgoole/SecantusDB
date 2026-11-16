//! `pull_up_simple_subquery` for an EXPLAINed SELECT (RP-422): a simple
//! FROM-subquery is merged into its parent, so `select * from (select *
//! from e where a > 5) s` plans as `select * from e where a > 5` -- `Seq
//! Scan on e` with `Filter: (a > 5)`, no Subquery Scan.
//!
//! Rewritten here: a parent whose one FROM item is a non-LATERAL
//! subquery with no column alias list, over a subquery that reads one
//! plain table with at most a WHERE, and whose output is `*` or plain
//! column references. Every parent reference must name one of the
//! subquery's columns. Anything else (a star over a column list, a
//! sublink, a set operation, an aggregate, a LIMIT) is left as written.

use super::*;
use pg_query::protobuf::{BoolExpr, BoolExprType, ColumnRef, Node, SelectStmt};

fn string_fields(c: &ColumnRef) -> Option<Vec<String>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

fn string_node(s: &str) -> Node {
    Node {
        node: Some(N::String(pg_query::protobuf::String { sval: s.into() })),
    }
}

/// A subquery `pull_up_simple_subquery` would merge: (its table's name in
/// the FROM, output name -> the column it reads).
fn simple(
    sub: &SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<(String, Vec<(String, String)>)> {
    let plain = sub.op == pg_query::protobuf::SetOperation::SetopNone as i32
        && sub.with_clause.is_none()
        && sub.group_clause.is_empty()
        && sub.having_clause.is_none()
        && sub.distinct_clause.is_empty()
        && sub.limit_count.is_none()
        && sub.limit_offset.is_none()
        && sub.sort_clause.is_empty()
        && sub.window_clause.is_empty()
        && sub.locking_clause.is_empty()
        && sub.into_clause.is_none()
        && sub.values_lists.is_empty();
    if !plain {
        return None;
    }
    let [item] = sub.from_clause.as_slice() else {
        return None;
    };
    let Some(N::RangeVar(rv)) = item.node.as_ref() else {
        return None;
    };
    if !rv.inh || rv.alias.as_ref().is_some_and(|a| !a.colnames.is_empty()) {
        return None;
    }
    let def = lookup(&rv.relname)?;
    let name = rv
        .alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .unwrap_or_else(|| rv.relname.clone());
    if let Some(w) = sub.where_clause.as_deref() {
        let mut sublink = false;
        let mut w = w.clone();
        walk_expr(&mut w, &mut |x| {
            sublink |= matches!(x.node.as_ref(), Some(N::SubLink(_)));
            Ok(())
        })
        .ok()?;
        if sublink {
            return None;
        }
    }
    let mut cols = Vec::new();
    for t in &sub.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return None;
        };
        let Some(N::ColumnRef(c)) = rt.val.as_deref().and_then(|v| v.node.as_ref()) else {
            return None;
        };
        match string_fields(c) {
            Some(parts) => {
                let col = match parts.as_slice() {
                    [col] => col,
                    [rel, col] if *rel == name => col,
                    _ => return None,
                };
                def.column(col)?;
                let out = if rt.name.is_empty() {
                    col.clone()
                } else {
                    rt.name.clone()
                };
                cols.push((out, col.clone()));
            }
            // `*` alone: the table's own columns.
            None if c.fields.len() == 1 && sub.target_list.len() == 1 => {
                for c in &def.columns {
                    cols.push((c.name.clone(), c.name.clone()));
                }
            }
            None => return None,
        }
    }
    // Two outputs of one name are ambiguous to the parent.
    for (i, (a, _)) in cols.iter().enumerate() {
        if cols[..i].iter().any(|(b, _)| a == b) {
            return None;
        }
    }
    Some((name, cols))
}

fn and_args(n: Node, out: &mut Vec<Node>) {
    if let Some(N::BoolExpr(b)) = n.node.as_ref() {
        if BoolExprType::try_from(b.boolop) == Ok(BoolExprType::AndExpr) {
            out.extend(b.args.iter().cloned());
            return;
        }
    }
    out.push(n);
}

/// Merge `query`'s one simple FROM-subquery into it. Leaves the statement
/// untouched unless every reference rewrites.
pub(crate) fn pull_up_simple_subquery(
    query: &mut N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    let N::SelectStmt(s) = query else {
        return Ok(());
    };
    if s.op != pg_query::protobuf::SetOperation::SetopNone as i32 || s.with_clause.is_some() {
        return Ok(());
    }
    let [item] = s.from_clause.as_slice() else {
        return Ok(());
    };
    let Some(N::RangeSubselect(r)) = item.node.as_ref() else {
        return Ok(());
    };
    let Some(alias) = r.alias.as_ref() else {
        return Ok(());
    };
    if r.lateral || !alias.colnames.is_empty() {
        return Ok(());
    }
    let Some(N::SelectStmt(sub)) = r.subquery.as_deref().and_then(|q| q.node.as_ref()) else {
        return Ok(());
    };
    let Some((name, cols)) = simple(sub, lookup) else {
        return Ok(());
    };
    let alias = alias.aliasname.clone();
    let star_inner = matches!(
        sub.target_list.as_slice(),
        [t] if matches!(
            t.node.as_ref(),
            Some(N::ResTarget(rt)) if matches!(
                rt.val.as_deref().and_then(|v| v.node.as_ref()),
                Some(N::ColumnRef(c)) if string_fields(c).is_none()
            )
        )
    );
    let mut copy = s.clone();
    let mut ok = true;
    // A bare column reference in the output keeps its name.
    for t in &mut copy.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if rt.name.is_empty() {
                if let Some(N::ColumnRef(c)) = rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                    if let Some(last) = string_fields(c).and_then(|p| p.last().cloned()) {
                        rt.name = last;
                    }
                }
            }
        }
    }
    let mut rewrite = |x: &mut Node| -> Result<()> {
        match x.node.as_mut() {
            Some(N::SubLink(_)) => ok = false,
            Some(N::ColumnRef(c)) => {
                let col = match string_fields(c) {
                    Some(parts) => match parts.as_slice() {
                        [col] => col.clone(),
                        [rel, col] if *rel == alias => col.clone(),
                        _ => {
                            ok = false;
                            return Ok(());
                        }
                    },
                    // `*` or `s.*`: only over the table's own star.
                    None => {
                        if !star_inner {
                            ok = false;
                        } else if c.fields.len() == 2 {
                            c.fields[0] = string_node(&name);
                        }
                        return Ok(());
                    }
                };
                match cols.iter().find(|(out, _)| *out == col) {
                    Some((_, inner)) => {
                        c.fields = vec![string_node(&name), string_node(inner)];
                    }
                    None => ok = false,
                }
            }
            _ => {}
        }
        Ok(())
    };
    for t in &mut copy.target_list {
        walk_expr(t, &mut rewrite)?;
    }
    if let Some(w) = copy.where_clause.as_deref_mut() {
        walk_expr(w, &mut rewrite)?;
    }
    if let Some(h) = copy.having_clause.as_deref_mut() {
        walk_expr(h, &mut rewrite)?;
    }
    for n in copy
        .group_clause
        .iter_mut()
        .chain(copy.sort_clause.iter_mut())
        .chain(copy.distinct_clause.iter_mut())
    {
        walk_expr(n, &mut rewrite)?;
    }
    if !ok || !copy.window_clause.is_empty() || !copy.locking_clause.is_empty() {
        return Ok(());
    }
    // The subquery's quals go first: `deconstruct_jointree` distributes the
    // lower jointree's before the parent's.
    let mut quals = Vec::new();
    if let Some(w) = sub.where_clause.as_deref() {
        and_args(w.clone(), &mut quals);
    }
    if let Some(w) = copy.where_clause.take() {
        and_args(*w, &mut quals);
    }
    copy.where_clause = match quals.len() {
        0 => None,
        1 => quals.pop().map(Box::new),
        _ => Some(Box::new(Node {
            node: Some(N::BoolExpr(Box::new(BoolExpr {
                xpr: None,
                boolop: BoolExprType::AndExpr as i32,
                args: quals,
                location: -1,
            }))),
        })),
    };
    copy.from_clause = sub.from_clause.clone();
    *s = copy;
    Ok(())
}
