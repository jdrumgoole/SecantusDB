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

/// A one-row constant subquery's `(alias, [(column, constant)])`. With
/// `unnamed`, a target without a name is allowed: it is `?column?` to the
/// parent, which no reference here rewrites, so its name is left empty.
fn constant_subquery(n: &Node, unnamed: bool) -> Option<(String, Vec<(String, Node)>)> {
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
        if !constant || untyped_string || (rt.name.is_empty() && !unnamed) {
            return None;
        }
        if !rt.name.is_empty() && cols.iter().any(|(c, _): &(String, Node)| *c == rt.name) {
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
                            match col.and_then(|col| {
                                cols.iter().find(|(c, _)| !c.is_empty() && c == col)
                            }) {
                                Some((_, v)) => *x = v.clone(),
                                None => ok = false,
                            }
                        }
                        // A bare name one of its columns has.
                        [Some(col)] if cols.iter().any(|(c, _)| c == col) => ok = false,
                        // `*`: the subquery's columns are in it.
                        [None] => ok = false,
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
    let Some((alias, cols)) = constant_subquery(&from[0], false) else {
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

/// A FROM item with its one-row constant subqueries taken out: each one
/// in the FROM list, or an arm of an inner join reached through inner
/// joins only, is gone (`remove_useless_result_rtes`), and that join's ON
/// is a WHERE conjunct now. `None`: the item was one itself.
fn strip_constant_items(
    item: &Node,
    removed: &mut Vec<(String, Vec<(String, Node)>)>,
    quals: &mut Vec<Node>,
) -> Option<Node> {
    if let Some(c) = constant_subquery(item, true) {
        removed.push(c);
        return None;
    }
    let Some(N::JoinExpr(j)) = item.node.as_ref() else {
        return Some(item.clone());
    };
    // A join alias, USING or NATURAL names the arms' columns itself.
    if j.jointype != pg_query::protobuf::JoinType::JoinInner as i32
        || j.is_natural
        || !j.using_clause.is_empty()
        || j.join_using_alias.is_some()
        || j.alias.is_some()
    {
        return Some(item.clone());
    }
    let (Some(l), Some(r)) = (j.larg.as_deref(), j.rarg.as_deref()) else {
        return Some(item.clone());
    };
    let (before, held) = (removed.len(), quals.len());
    let l = strip_constant_items(l, removed, quals);
    let r = strip_constant_items(r, removed, quals);
    match (l, r) {
        (Some(l), Some(r)) => {
            let mut j = j.clone();
            j.larg = Some(Box::new(l));
            j.rarg = Some(Box::new(r));
            Some(Node {
                node: Some(N::JoinExpr(j)),
            })
        }
        (Some(side), None) | (None, Some(side)) => {
            if let Some(q) = j.quals.as_deref() {
                quals.insert(held, q.clone());
            }
            Some(side)
        }
        (None, None) => {
            removed.truncate(before);
            quals.truncate(held);
            Some(item.clone())
        }
    }
}

/// `pull_up_subqueries` of the one-row constant subqueries in an
/// EXPLAINed SELECT's FROM (RP-1107): `select e.b, count(*) from e,
/// (select 1) x group by e.b` plans as `... from e group by e.b`. Each
/// becomes an RTE_RESULT, its columns its constants, and
/// `remove_useless_result_rtes` takes it out of the join tree; it stays in
/// the range table, which the caller says by planning the result with
/// [`crate::join_removal::with_joins_removed`] (the keys print qualified).
///
/// Only when every reference to one is a qualified name of one of its
/// columns outside GROUP BY, ORDER BY and DISTINCT ON (where a constant
/// would read as a column position), no `*` covers it, and at least one
/// other FROM item is left. `None` leaves the statement as written.
pub(crate) fn pull_up_select_constant_subqueries(query: &N) -> Option<N> {
    let N::SelectStmt(s) = query else {
        return None;
    };
    if s.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || s.with_clause.is_some()
        || s.into_clause.is_some()
        || !s.locking_clause.is_empty()
        || !s.window_clause.is_empty()
        || !s.values_lists.is_empty()
    {
        return None;
    }
    let (mut removed, mut quals) = (Vec::new(), Vec::new());
    let from: Vec<Node> = s
        .from_clause
        .iter()
        .filter_map(|item| strip_constant_items(item, &mut removed, &mut quals))
        .collect();
    if removed.is_empty() || from.is_empty() {
        return None;
    }
    let mut copy = s.clone();
    copy.from_clause = from;
    // A bare column reference in the output keeps its name.
    for t in &mut copy.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if rt.name.is_empty() {
                if let Some(N::ColumnRef(c)) = rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                    if let Some(Some(N::String(last))) = c.fields.last().map(|f| f.node.as_ref()) {
                        rt.name = last.sval.clone();
                    }
                }
            }
        }
    }
    if let Some(w) = copy.where_clause.take() {
        quals.push(*w);
    }
    copy.where_clause = match quals.len() {
        0 => None,
        1 => quals.pop().map(Box::new),
        _ => Some(Box::new(Node {
            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                xpr: None,
                boolop: pg_query::protobuf::BoolExprType::AndExpr as i32,
                args: quals,
                location: -1,
            }))),
        })),
    };
    for (alias, cols) in &removed {
        // Another item's ON, or a key clause, that reads it: as written.
        let mut keyed: Vec<Node> = copy
            .group_clause
            .iter()
            .chain(&copy.sort_clause)
            .chain(&copy.distinct_clause)
            .cloned()
            .collect();
        for item in &copy.from_clause {
            join_quals(item, &mut keyed);
        }
        let before = keyed.clone();
        let mut nodes: Vec<&mut Node> = keyed.iter_mut().collect();
        if !substitute(&mut nodes, alias, cols).ok()? || keyed != before {
            return None;
        }
        let mut nodes: Vec<&mut Node> = copy.target_list.iter_mut().collect();
        if let Some(w) = copy.where_clause.as_deref_mut() {
            nodes.push(w);
        }
        if let Some(h) = copy.having_clause.as_deref_mut() {
            nodes.push(h);
        }
        if !substitute(&mut nodes, alias, cols).ok()? {
            return None;
        }
        // A reference in a node the walk does not descend into would be
        // left naming a relation no longer in the FROM: anything still
        // spelling the alias (a string constant of that text included)
        // leaves the statement as written.
        let spelled = format!("sval: {alias:?}");
        let rest = format!(
            "{:?}{:?}{:?}{:?}",
            copy.target_list, copy.where_clause, copy.having_clause, copy.from_clause
        );
        if rest.contains(&spelled) {
            return None;
        }
    }
    Some(N::SelectStmt(copy))
}

/// The ON clauses of the joins in `item`.
fn join_quals(item: &Node, out: &mut Vec<Node>) {
    if let Some(N::JoinExpr(j)) = item.node.as_ref() {
        if let Some(q) = j.quals.as_deref() {
            out.push(q.clone());
        }
        for side in [&j.larg, &j.rarg].into_iter().flatten() {
            join_quals(side, out);
        }
    }
}

/// An `UPDATE ... FROM` or `DELETE ... USING` with a FROM item left, as the
/// join PostgreSQL plans it under its ModifyTable (RP-435): `SELECT <SET values> FROM
/// <target>, <from items> WHERE <where>`, the target first as it is the
/// range table's first entry. Returns the ModifyTable node's name (`Delete
/// on k`), or `None` (and leaves the statement) when there is no join.
/// Only EXPLAIN without ANALYZE uses it: the statement is not run.
pub(crate) fn dml_join_as_select(query: &mut N) -> Option<String> {
    let (verb, relation, from, where_clause) = match &*query {
        N::UpdateStmt(u) if u.with_clause.is_none() && !u.from_clause.is_empty() => (
            "Update",
            u.relation.as_ref()?,
            &u.from_clause,
            &u.where_clause,
        ),
        N::DeleteStmt(d) if d.with_clause.is_none() && !d.using_clause.is_empty() => (
            "Delete",
            d.relation.as_ref()?,
            &d.using_clause,
            &d.where_clause,
        ),
        _ => return None,
    };
    let name = match &relation.alias {
        Some(a) => format!("{verb} on {} {}", relation.relname, a.aliasname),
        None => format!("{verb} on {}", relation.relname),
    };
    // The join's target list is what the SET list reads; the row identity
    // (`ctid`) each relation adds is the plan's (`add_row_identity`).
    let target_list: Vec<Node> = match &*query {
        N::UpdateStmt(u) => u
            .target_list
            .iter()
            .filter_map(|t| match t.node.as_ref() {
                Some(N::ResTarget(r)) => Some(Node {
                    node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                        val: r.val.clone(),
                        location: -1,
                        ..Default::default()
                    }))),
                }),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut from_clause = vec![Node {
        node: Some(N::RangeVar(relation.clone())),
    }];
    from_clause.extend(from.iter().cloned());
    let select = pg_query::protobuf::SelectStmt {
        target_list,
        from_clause,
        where_clause: where_clause.clone(),
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        ..Default::default()
    };
    *query = N::SelectStmt(Box::new(select));
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pulled(sql: &str) -> Option<String> {
        let parsed = pg_query::parse(sql).unwrap();
        let q = parsed.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .clone()
            .unwrap();
        pull_up_select_constant_subqueries(&q).map(|n| Node { node: Some(n) }.deparse().unwrap())
    }

    /// A one-row constant subquery beside another FROM item leaves the
    /// join tree, its columns its constants; a bare output keeps its name
    /// (RP-1107).
    #[test]
    fn a_constant_subquery_beside_another_item_is_pulled_up() {
        assert_eq!(
            pulled("select e.b, count(*) from e, (select 1) x group by e.b").as_deref(),
            Some("SELECT e.b AS b, count(*) FROM e GROUP BY e.b")
        );
        assert_eq!(
            pulled("select e.b, x.c from e join (select 3 as c) x on e.a = x.c").as_deref(),
            Some("SELECT e.b AS b, 3 AS c FROM e WHERE e.a = 3")
        );
        assert_eq!(
            pulled("select e.b from (select 1 c) x join e on e.a = x.c join k on k.a = e.a")
                .as_deref(),
            Some("SELECT e.b AS b FROM e JOIN k ON k.a = e.a WHERE e.a = 1")
        );
    }

    /// One a star covers, read bare, in a key clause or a sublink, with a
    /// WHERE or a non-literal target, or alone in the FROM, is not.
    #[test]
    fn a_constant_subquery_read_otherwise_is_left_as_written() {
        for sql in [
            "select * from e, (select 1 c) x",
            "select e.b, c from e, (select 1 c) x",
            "select e.b from e, (select 1 c) x order by x.c",
            "select e.b from e, (select 1 c) x group by x.c, e.b",
            "select e.b from e, (select 1 c) x where e.a in (select a from k where k.a = x.c)",
            "select e.b from e, (select 1 where false) x",
            "select e.b from e, (select random() r) x",
            "select e.b from e, (select 1 + 1 c) x",
            "select x.c from (select 1 c) x",
            "select e.b from e left join (select 1 c) x on true",
            "select e.b from e, (select 1 c) x, lateral (select x.c) y",
        ] {
            assert_eq!(pulled(sql), None, "{sql}");
        }
    }
}
