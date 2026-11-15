//! `pull_up_subqueries` of a one-row constant FROM subquery in an
//! EXPLAINed UPDATE ... FROM or DELETE ... USING (RP-435): `delete from k
//! using (select 50 as a) t where k.a = t.a` plans as `delete from k where
//! k.a = 50` -- the subquery becomes an RTE_RESULT and its columns its
//! constants, so the target's key is an Index Cond.
//!
//! Only a subquery whose targets are all named constants (no FROM, WHERE,
//! GROUP BY, set operation, LIMIT ...) is pulled up, and only when every
//! reference to it is qualified by its alias.

use super::*;
use pg_query::protobuf::Node;

/// A one-row constant subquery's `(alias, [(column, constant)])`.
fn constant_subquery(n: &Node) -> Option<(String, Vec<(String, Node)>)> {
    let Some(N::RangeSubselect(r)) = n.node.as_ref() else {
        return None;
    };
    if r.lateral {
        return None;
    }
    let alias = r.alias.as_ref()?;
    if !alias.colnames.is_empty() {
        return None;
    }
    let Some(N::SelectStmt(s)) = r.subquery.as_deref()?.node.as_ref() else {
        return None;
    };
    if !s.from_clause.is_empty()
        || s.where_clause.is_some()
        || s.having_clause.is_some()
        || !s.group_clause.is_empty()
        || !s.distinct_clause.is_empty()
        || !s.sort_clause.is_empty()
        || s.limit_count.is_some()
        || s.limit_offset.is_some()
        || s.with_clause.is_some()
        || !s.values_lists.is_empty()
        || s.op != pg_query::protobuf::SetOperation::SetopNone as i32
    {
        return None;
    }
    let mut cols = Vec::new();
    for t in &s.target_list {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            return None;
        };
        let v = rt.val.as_deref()?;
        // A literal, or a literal cast: a Const after planning.
        let constant = match v.node.as_ref()? {
            N::AConst(c) => !c.isnull,
            N::TypeCast(tc) => matches!(
                tc.arg.as_deref().and_then(|a| a.node.as_ref()),
                Some(N::AConst(c)) if !c.isnull
            ),
            _ => false,
        };
        // An unnamed or untyped string literal keeps its own type rules.
        let untyped_string = matches!(
            v.node.as_ref(),
            Some(N::AConst(c)) if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
        );
        if !constant || untyped_string || rt.name.is_empty() {
            return None;
        }
        if cols.iter().any(|(c, _): &(String, Node)| *c == rt.name) {
            return None;
        }
        cols.push((rt.name.clone(), v.clone()));
    }
    Some((alias.aliasname.clone(), cols))
}

/// Substitute the subquery's columns in `nodes`; false when a reference
/// to it is not one of them (or a star, or inside a subquery).
fn substitute(nodes: &mut [&mut Node], alias: &str, cols: &[(String, Node)]) -> Result<bool> {
    let mut ok = true;
    for n in nodes.iter_mut() {
        walk_expr(n, &mut |x| {
            match x.node.as_ref() {
                Some(N::SubLink(_)) => ok = false,
                Some(N::ColumnRef(c)) => {
                    let parts: Vec<Option<&str>> = c
                        .fields
                        .iter()
                        .map(|f| match f.node.as_ref() {
                            Some(N::String(s)) => Some(s.sval.as_str()),
                            _ => None,
                        })
                        .collect();
                    match parts.as_slice() {
                        [Some(rel), col] if *rel == alias => {
                            match col.and_then(|col| cols.iter().find(|(c, _)| c == col)) {
                                Some((_, v)) => *x = v.clone(),
                                None => ok = false,
                            }
                        }
                        // A bare name one of its columns has.
                        [Some(col)] if cols.iter().any(|(c, _)| c == col) => ok = false,
                        _ => {}
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
    }
    Ok(ok)
}

pub(crate) fn pull_up_constant_subqueries(query: &mut N) -> Result<()> {
    let (from, has_returning) = match query {
        N::UpdateStmt(u) if u.with_clause.is_none() => {
            (&u.from_clause, !u.returning_list.is_empty())
        }
        N::DeleteStmt(d) if d.with_clause.is_none() => {
            (&d.using_clause, !d.returning_list.is_empty())
        }
        _ => return Ok(()),
    };
    if from.len() != 1 || has_returning {
        return Ok(());
    }
    let Some((alias, cols)) = constant_subquery(&from[0]) else {
        return Ok(());
    };
    let mut copy = query.clone();
    let ok = match &mut copy {
        N::UpdateStmt(u) => {
            let mut nodes: Vec<&mut Node> = u.target_list.iter_mut().collect();
            if let Some(w) = u.where_clause.as_deref_mut() {
                nodes.push(w);
            }
            let ok = substitute(&mut nodes, &alias, &cols)?;
            u.from_clause.clear();
            ok
        }
        N::DeleteStmt(d) => {
            let mut nodes: Vec<&mut Node> = Vec::new();
            if let Some(w) = d.where_clause.as_deref_mut() {
                nodes.push(w);
            }
            let ok = substitute(&mut nodes, &alias, &cols)?;
            d.using_clause.clear();
            ok
        }
        _ => false,
    };
    if ok {
        *query = copy;
    }
    Ok(())
}
