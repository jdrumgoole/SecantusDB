//! `flatten_join_alias_vars` for an EXPLAINed SELECT (RP-471): a join
//! alias's columns are the sides' own, so `select j.c from (fs join fe on
//! fs.a = fe.a) as j where j.b = 'x'` plans as the unaliased query --
//! `j.b = 'x'` is `fe`'s scan filter and `fs.a = fe.a` the Hash Cond.
//!
//! Rewritten here: an aliased inner (or cross) JOIN without USING, NATURAL
//! or a column alias list, over plain tables, whose alias each column
//! reference names a column exactly one side has. Anything else (a `j.*`,
//! a subquery that might name the alias, an ambiguous column) is left as
//! written.

use super::*;
use pg_query::protobuf::Node;

/// The relations under a join tree: (the name each goes by, its table).
fn sides(
    n: &Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<(String, TableDef)>,
) -> Option<()> {
    match n.node.as_ref()? {
        N::RangeVar(rv) => {
            if rv.alias.as_ref().is_some_and(|a| !a.colnames.is_empty()) {
                return None;
            }
            let def = lookup(&rv.relname)?;
            let name = rv
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_else(|| rv.relname.clone());
            out.push((name, def));
            Some(())
        }
        N::JoinExpr(j) => {
            if j.alias.is_some() {
                return None;
            }
            plain_inner(j)?;
            sides(j.larg.as_deref()?, lookup, out)?;
            sides(j.rarg.as_deref()?, lookup, out)
        }
        _ => None,
    }
}

fn plain_inner(j: &pg_query::protobuf::JoinExpr) -> Option<()> {
    (j.jointype == pg_query::protobuf::JoinType::JoinInner as i32
        && !j.is_natural
        && j.using_clause.is_empty())
    .then_some(())
}

fn string_fields(c: &pg_query::protobuf::ColumnRef) -> Option<Vec<String>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// Flatten every aliased join's references in `query`. Leaves the
/// statement untouched unless every reference to every alias rewrites.
pub(crate) fn flatten_join_aliases(
    query: &mut N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    let N::SelectStmt(s) = query else {
        return Ok(());
    };
    if s.op != pg_query::protobuf::SetOperation::SetopNone as i32 || s.with_clause.is_some() {
        return Ok(());
    }
    // alias -> its relations.
    let mut aliases: Vec<(String, Vec<(String, TableDef)>)> = Vec::new();
    for f in &s.from_clause {
        if let Some(N::JoinExpr(j)) = f.node.as_ref() {
            if let Some(a) = j.alias.as_ref() {
                if !a.colnames.is_empty() || plain_inner(j).is_none() {
                    return Ok(());
                }
                let mut rels = Vec::new();
                let ok = j
                    .larg
                    .as_deref()
                    .and_then(|l| sides(l, lookup, &mut rels))
                    .and(j.rarg.as_deref())
                    .and_then(|r| sides(r, lookup, &mut rels));
                if ok.is_none() {
                    return Ok(());
                }
                aliases.push((a.aliasname.clone(), rels));
            }
        }
    }
    if aliases.is_empty() {
        return Ok(());
    }
    // Unaliased, a side's name must not meet another FROM item's.
    let mut names: Vec<String> = Vec::new();
    for f in &s.from_clause {
        let mut rels = Vec::new();
        let own = match f.node.as_ref() {
            Some(N::JoinExpr(j)) if j.alias.is_some() => {
                let _ = j.larg.as_deref().map(|l| sides(l, lookup, &mut rels));
                let _ = j.rarg.as_deref().map(|r| sides(r, lookup, &mut rels));
                rels.into_iter().map(|(n, _)| n).collect::<Vec<_>>()
            }
            Some(N::JoinExpr(_)) => {
                if sides(f, lookup, &mut rels).is_none() {
                    return Ok(());
                }
                rels.into_iter().map(|(n, _)| n).collect()
            }
            Some(N::RangeVar(rv)) => vec![rv
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_else(|| rv.relname.clone())],
            Some(N::RangeSubselect(r)) => r.alias.iter().map(|a| a.aliasname.clone()).collect(),
            _ => return Ok(()),
        };
        for n in own {
            if names.contains(&n) || aliases.iter().any(|(a, _)| *a == n) {
                return Ok(());
            }
            names.push(n);
        }
    }
    let mut copy = s.clone();
    let mut ok = true;
    let mut rewrite = |x: &mut Node| -> Result<()> {
        match x.node.as_mut() {
            Some(N::SubLink(_)) => ok = false,
            Some(N::ColumnRef(c)) => {
                let Some(parts) = string_fields(c) else {
                    // `j.*` or `*`: a star over an alias is not flattened.
                    if c.fields.len() == 2 {
                        ok = false;
                    }
                    return Ok(());
                };
                let [rel, col] = parts.as_slice() else {
                    return Ok(());
                };
                let Some((_, rels)) = aliases.iter().find(|(a, _)| a == rel) else {
                    return Ok(());
                };
                let mut owners = rels.iter().filter(|(_, def)| def.column(col).is_some());
                match (owners.next(), owners.next()) {
                    (Some((name, _)), None) => {
                        c.fields[0] = Node {
                            node: Some(N::String(pg_query::protobuf::String {
                                sval: name.clone(),
                            })),
                        };
                    }
                    _ => ok = false,
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
    {
        walk_expr(n, &mut rewrite)?;
    }
    if !copy.window_clause.is_empty() || !copy.distinct_clause.is_empty() {
        return Ok(());
    }
    if !ok {
        return Ok(());
    }
    // The join itself loses its alias: its relations are named as their
    // own.
    for f in &mut copy.from_clause {
        if let Some(N::JoinExpr(j)) = f.node.as_mut() {
            j.alias = None;
        }
    }
    *s = copy;
    Ok(())
}
