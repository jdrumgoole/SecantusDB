//! A view's stored definition is SQL TEXT, where PostgreSQL stores a query
//! tree bound to its relations and columns by OID. So a rename that
//! PostgreSQL shrugs off -- a table, a column, the view itself -- would leave
//! the text naming something that no longer exists, and every read of the
//! view would fail. These rewrite the text the way the binding would have
//! followed the rename.

use super::*;

fn parse_select(sql: &str) -> Result<pg_query::ParseResult> {
    pg_query::parse(sql).map_err(|e| Error::Parse(e.to_string()))
}

fn deparse(tree: &pg_query::ParseResult) -> Result<String> {
    tree.deparse().map_err(|e| Error::Parse(e.to_string()))
}

/// `sql` with every reference to relation `old` (unqualified or `public.`)
/// renamed to `new` -- the FROM items, and a column reference qualified by
/// the bare relation name (`old.col`).
pub fn rename_relation(sql: &str, old: &str, new: &str) -> Result<String> {
    let mut tree = parse_select(sql)?;
    let mut changed = false;
    // SAFETY: the pointers `nodes_mut` hands out point into `tree`, which
    // outlives this loop and is not otherwise touched while they are used.
    unsafe {
        for (node, _, _) in tree.protobuf.nodes_mut() {
            match node {
                pg_query::NodeMut::RangeVar(r) => {
                    let r = &mut *r;
                    if r.relname == old && (r.schemaname.is_empty() || r.schemaname == "public") {
                        r.relname = new.to_string();
                        changed = true;
                    }
                }
                pg_query::NodeMut::ColumnRef(c) => {
                    let c = &mut *c;
                    let n = c.fields.len();
                    if n >= 2 {
                        if let Some(N::String(q)) = c.fields[n - 2].node.as_mut() {
                            if q.sval == old {
                                q.sval = new.to_string();
                                changed = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if changed {
        deparse(&tree)
    } else {
        Ok(sql.to_string())
    }
}

/// A view definition whose output column `index` (of `names`, the current
/// output names in order) is renamed to `new`. A select list without a `*`
/// renames the entry in place; otherwise the whole definition is wrapped
/// with a column list, the form `CREATE VIEW v (cols)` already stores.
pub fn rename_output(
    definition: &str,
    names: &[String],
    index: usize,
    new: &str,
) -> Result<String> {
    let mut tree = parse_select(definition)?;
    let in_place = (|| {
        let stmt = tree.protobuf.stmts.first_mut()?.stmt.as_mut()?;
        let Some(N::SelectStmt(s)) = stmt.node.as_mut() else {
            return None;
        };
        let plain = s.op == pg_query::protobuf::SetOperation::SetopNone as i32
            && s.values_lists.is_empty()
            && s.target_list.len() == names.len()
            && !s.target_list.iter().any(|t| {
                matches!(t.node.as_ref(), Some(N::ResTarget(rt))
                    if matches!(rt.val.as_deref().and_then(|v| v.node.as_ref()),
                        Some(N::ColumnRef(c)) if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_))))))
            });
        if !plain {
            return None;
        }
        let Some(N::ResTarget(rt)) = s.target_list.get_mut(index)?.node.as_mut() else {
            return None;
        };
        rt.name = new.to_string();
        Some(())
    })();
    if in_place.is_some() {
        return deparse(&tree);
    }
    let q = crate::scalar::quote_identifier;
    let cols: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(i, n)| q(if i == index { new } else { n }))
        .collect();
    Ok(format!(
        "SELECT * FROM ({definition}) AS {}({})",
        q("v"),
        cols.join(", ")
    ))
}

/// One FROM item as a column reference resolves against it.
struct FromItem {
    /// What qualifies its columns: the alias, else the relation name.
    qualifier: String,
    /// The relation it reads, when it is a plain one.
    relname: Option<String>,
    /// Its column names, when known.
    columns: Option<Vec<String>>,
}

/// The column rename a view's text has to follow.
struct ColumnRename<'a> {
    relation: &'a str,
    old: &'a str,
    new: &'a str,
    columns_of: &'a dyn Fn(&str) -> Option<Vec<String>>,
}

/// `sql` with each reference to column `old` of relation `relation` renamed
/// to `new`, resolved the way the binding would have: a qualified reference
/// by its qualifier, an unqualified one by the innermost query level whose
/// FROM has a column of that name -- and only when that is `relation` alone.
/// A select-list entry that loses its name to the rename keeps it (`new AS
/// old`), because a view's output columns are fixed when it is created.
///
/// `columns_of` answers a relation's columns BEFORE the rename.
pub fn rename_column(
    sql: &str,
    relation: &str,
    old: &str,
    new: &str,
    columns_of: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<String> {
    let mut tree = parse_select(sql)?;
    let r = ColumnRename {
        relation,
        old,
        new,
        columns_of,
    };
    let mut changed = false;
    for raw in &mut tree.protobuf.stmts {
        if let Some(N::SelectStmt(s)) = raw.stmt.as_mut().and_then(|n| n.node.as_mut()) {
            changed |= rename_in_select(s, &r, &mut Vec::new())?;
        }
    }
    if changed {
        deparse(&tree)
    } else {
        Ok(sql.to_string())
    }
}

fn from_items(
    item: &mut pg_query::protobuf::Node,
    r: &ColumnRename,
    outer: &mut Vec<Vec<FromItem>>,
    out: &mut Vec<FromItem>,
    changed: &mut bool,
) -> Result<()> {
    match item.node.as_mut() {
        Some(N::RangeVar(v)) => {
            let qualifier = v
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| v.relname.clone());
            out.push(FromItem {
                qualifier,
                relname: Some(v.relname.clone()),
                columns: (r.columns_of)(&v.relname),
            });
        }
        Some(N::RangeSubselect(rs)) => {
            let mut names = None;
            if let Some(N::SelectStmt(s)) = rs.subquery.as_deref_mut().and_then(|q| q.node.as_mut())
            {
                *changed |= rename_in_select(s, r, outer)?;
                names = Some(output_names(s));
            }
            let alias = rs.alias.as_ref();
            if let Some(cols) = alias.filter(|a| !a.colnames.is_empty()).map(|a| {
                a.colnames
                    .iter()
                    .filter_map(|c| match c.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            }) {
                names = Some(cols);
            }
            out.push(FromItem {
                qualifier: alias.map(|a| a.aliasname.clone()).unwrap_or_default(),
                relname: None,
                columns: names,
            });
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                from_items(side, r, outer, out, changed)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// A select list's output names, as far as the text says them.
fn output_names(s: &pg_query::protobuf::SelectStmt) -> Vec<String> {
    s.target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) if !rt.name.is_empty() => Some(rt.name.clone()),
            Some(N::ResTarget(rt)) => match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                Some(N::ColumnRef(c)) => c.fields.last().and_then(|f| match f.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.clone()),
                    _ => None,
                }),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Whether column reference `c` names `r.old` of `r.relation`, given the
/// FROM items in scope (innermost last).
fn refers(c: &pg_query::protobuf::ColumnRef, r: &ColumnRename, scopes: &[Vec<FromItem>]) -> bool {
    let parts: Vec<&str> = c
        .fields
        .iter()
        .filter_map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    if parts.len() != c.fields.len() || parts.last() != Some(&r.old) {
        return false;
    }
    let is_rel = |i: &FromItem| i.relname.as_deref() == Some(r.relation);
    match parts.as_slice() {
        [_] => {
            for level in scopes.iter().rev() {
                let having: Vec<&FromItem> = level
                    .iter()
                    .filter(|i| {
                        i.columns
                            .as_ref()
                            .is_some_and(|cs| cs.iter().any(|x| x == r.old))
                    })
                    .collect();
                if !having.is_empty() {
                    return having.len() == 1 && is_rel(having[0]);
                }
            }
            false
        }
        [.., q, _] => {
            for level in scopes.iter().rev() {
                if let Some(item) = level.iter().find(|i| i.qualifier == *q) {
                    return is_rel(item);
                }
            }
            false
        }
        [] => false,
    }
}

fn rename_ref(n: &mut pg_query::protobuf::Node, r: &ColumnRename) {
    if let Some(N::ColumnRef(c)) = n.node.as_mut() {
        if let Some(N::String(s)) = c.fields.last_mut().and_then(|f| f.node.as_mut()) {
            s.sval = r.new.to_string();
        }
    }
}

fn rename_in_expr(
    node: &mut pg_query::protobuf::Node,
    r: &ColumnRename,
    scopes: &mut Vec<Vec<FromItem>>,
) -> Result<bool> {
    let mut changed = false;
    walk_expr(node, &mut |n| {
        match n.node.as_mut() {
            Some(N::ColumnRef(c)) => {
                if refers(c, r, scopes) {
                    rename_ref(n, r);
                    changed = true;
                }
            }
            Some(N::SubLink(sl)) => {
                if let Some(N::SelectStmt(s)) =
                    sl.subselect.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    changed |= rename_in_select(s, r, scopes)?;
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(changed)
}

fn rename_in_select(
    s: &mut pg_query::protobuf::SelectStmt,
    r: &ColumnRename,
    scopes: &mut Vec<Vec<FromItem>>,
) -> Result<bool> {
    let mut changed = false;
    for side in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        changed |= rename_in_select(side, r, scopes)?;
    }
    if let Some(with) = s.with_clause.as_mut() {
        for cte in &mut with.ctes {
            if let Some(N::CommonTableExpr(c)) = cte.node.as_mut() {
                if let Some(N::SelectStmt(q)) =
                    c.ctequery.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    changed |= rename_in_select(q, r, scopes)?;
                }
            }
        }
    }
    let mut level = Vec::new();
    for item in &mut s.from_clause {
        from_items(item, r, scopes, &mut level, &mut changed)?;
    }
    scopes.push(level);
    let result = (|| -> Result<bool> {
        let mut changed = false;
        for item in &mut s.from_clause {
            changed |= rename_in_join_quals(item, r, scopes)?;
        }
        for t in &mut s.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_mut() else {
                continue;
            };
            let bare = matches!(rt.val.as_deref().and_then(|v| v.node.as_ref()),
                Some(N::ColumnRef(c)) if refers(c, r, scopes));
            if bare && rt.name.is_empty() {
                rt.name = r.old.to_string();
            }
            if let Some(v) = rt.val.as_deref_mut() {
                changed |= rename_in_expr(v, r, scopes)?;
            }
        }
        for clause in [
            s.where_clause.as_deref_mut(),
            s.having_clause.as_deref_mut(),
        ]
        .into_iter()
        .flatten()
        {
            changed |= rename_in_expr(clause, r, scopes)?;
        }
        for g in s.group_clause.iter_mut() {
            changed |= rename_in_expr(g, r, scopes)?;
        }
        for item in &mut s.sort_clause {
            if let Some(N::SortBy(sb)) = item.node.as_mut() {
                if let Some(n) = sb.node.as_deref_mut() {
                    changed |= rename_in_expr(n, r, scopes)?;
                }
            }
        }
        Ok(changed)
    })();
    scopes.pop();
    Ok(changed | result?)
}

fn rename_in_join_quals(
    item: &mut pg_query::protobuf::Node,
    r: &ColumnRename,
    scopes: &mut Vec<Vec<FromItem>>,
) -> Result<bool> {
    let mut changed = false;
    if let Some(N::JoinExpr(j)) = item.node.as_mut() {
        for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
            .into_iter()
            .flatten()
        {
            changed |= rename_in_join_quals(side, r, scopes)?;
        }
        if let Some(q) = j.quals.as_deref_mut() {
            changed |= rename_in_expr(q, r, scopes)?;
        }
    }
    Ok(changed)
}

/// A view definition with its `*` and `t.*` select-list entries expanded
/// into the columns they cover NOW, as PostgreSQL expands them when the view
/// is created -- so a column later added to a table does not appear in the
/// view, and a later column rename has a name to follow.
///
/// Left as written where the expansion cannot be exact: a `*` over a
/// `USING` / `NATURAL` join (which merges the joined columns), or over a
/// FROM item whose columns are not known.
pub fn expand_stars(sql: &str, columns_of: &dyn Fn(&str) -> Option<Vec<String>>) -> Result<String> {
    let mut tree = parse_select(sql)?;
    let mut changed = false;
    for raw in &mut tree.protobuf.stmts {
        if let Some(N::SelectStmt(s)) = raw.stmt.as_mut().and_then(|n| n.node.as_mut()) {
            changed |= expand_in_select(s, columns_of)?;
        }
    }
    if changed {
        deparse(&tree)
    } else {
        Ok(sql.to_string())
    }
}

fn is_star(c: &pg_query::protobuf::ColumnRef) -> bool {
    matches!(
        c.fields.last().and_then(|f| f.node.as_ref()),
        Some(N::AStar(_))
    )
}

fn expand_in_select(
    s: &mut pg_query::protobuf::SelectStmt,
    columns_of: &dyn Fn(&str) -> Option<Vec<String>>,
) -> Result<bool> {
    let mut changed = false;
    for side in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        changed |= expand_in_select(side, columns_of)?;
    }
    // `SELECT * FROM (body) AS v(cols)` is how a declared column list is
    // stored: the outer `*` stays (it is what renames), the body expands.
    if let [item] = s.from_clause.as_mut_slice() {
        if let Some(N::RangeSubselect(rs)) = item.node.as_mut() {
            if rs.alias.as_ref().is_some_and(|a| !a.colnames.is_empty()) {
                if let Some(N::SelectStmt(inner)) =
                    rs.subquery.as_deref_mut().and_then(|q| q.node.as_mut())
                {
                    return Ok(changed | expand_in_select(inner, columns_of)?);
                }
            }
        }
    }
    let rewrite = |q: &str, c: &str| pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            val: Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                    fields: [q, c]
                        .iter()
                        .map(|p| pg_query::protobuf::Node {
                            node: Some(N::String(pg_query::protobuf::String {
                                sval: p.to_string(),
                            })),
                        })
                        .collect(),
                    location: -1,
                })),
            })),
            location: -1,
            ..Default::default()
        }))),
    };
    let has_star = s.target_list.iter().any(|t| {
        matches!(t.node.as_ref(), Some(N::ResTarget(rt))
            if matches!(rt.val.as_deref().and_then(|v| v.node.as_ref()), Some(N::ColumnRef(c)) if is_star(c)))
    });
    if !has_star {
        return Ok(changed);
    }
    // The FROM items, in order, with their columns; `None` for the whole
    // list when a `*` over them could not be expanded exactly.
    fn collect(
        item: &pg_query::protobuf::Node,
        columns_of: &dyn Fn(&str) -> Option<Vec<String>>,
        out: &mut Vec<(String, Option<Vec<String>>)>,
        merged: &mut bool,
    ) {
        match item.node.as_ref() {
            Some(N::RangeVar(v)) => {
                let q = v
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| v.relname.clone());
                out.push((q, columns_of(&v.relname)));
            }
            Some(N::RangeSubselect(rs)) => {
                let q = rs
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_default();
                let cols = match rs.subquery.as_deref().and_then(|q| q.node.as_ref()) {
                    // A VALUES list's columns are `column1..N`; it has no
                    // select list to read them from.
                    Some(N::SelectStmt(sub)) if !sub.values_lists.is_empty() => {
                        match sub.values_lists.first().and_then(|r| r.node.as_ref()) {
                            Some(N::List(l)) => Some(
                                (1..=l.items.len()).map(|i| format!("column{i}")).collect(),
                            ),
                            _ => None,
                        }
                    }
                    Some(N::SelectStmt(sub)) if !sub.target_list.iter().any(|t| {
                        matches!(t.node.as_ref(), Some(N::ResTarget(rt))
                            if matches!(rt.val.as_deref().and_then(|v| v.node.as_ref()), Some(N::ColumnRef(c)) if is_star(c)))
                    }) && sub.op == pg_query::protobuf::SetOperation::SetopNone as i32 =>
                    {
                        let names = output_names(sub);
                        (names.len() == sub.target_list.len()).then_some(names)
                    }
                    _ => None,
                };
                let cols = match rs.alias.as_ref().filter(|a| !a.colnames.is_empty()) {
                    Some(a) => cols.map(|mut c| {
                        for (i, n) in a.colnames.iter().enumerate() {
                            if let (Some(slot), Some(N::String(s))) =
                                (c.get_mut(i), n.node.as_ref())
                            {
                                *slot = s.sval.clone();
                            }
                        }
                        c
                    }),
                    None => cols,
                };
                out.push((q, cols));
            }
            Some(N::JoinExpr(j)) => {
                if j.is_natural || !j.using_clause.is_empty() {
                    *merged = true;
                }
                for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                    collect(side, columns_of, out, merged);
                }
            }
            _ => out.push((String::new(), None)),
        }
    }
    let mut items = Vec::new();
    let mut merged = false;
    for item in &s.from_clause {
        collect(item, columns_of, &mut items, &mut merged);
    }
    let mut out = Vec::new();
    let mut expanded = false;
    for t in std::mem::take(&mut s.target_list) {
        let star = match t.node.as_ref() {
            Some(N::ResTarget(rt)) => match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                Some(N::ColumnRef(c)) if is_star(c) => Some(c.clone()),
                _ => None,
            },
            _ => None,
        };
        let Some(c) = star else {
            out.push(t);
            continue;
        };
        let qualifier = match c.fields.as_slice() {
            [_] => None,
            [.., q, _] => match q.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            },
            [] => None,
        };
        let chosen: Vec<&(String, Option<Vec<String>>)> = match &qualifier {
            None if merged => Vec::new(),
            None => items.iter().collect(),
            Some(q) => items.iter().filter(|(n, _)| n == q).collect(),
        };
        if chosen.is_empty() || chosen.iter().any(|(q, c)| c.is_none() || q.is_empty()) {
            out.push(t);
            continue;
        }
        for (q, cols) in chosen {
            for col in cols.as_ref().expect("checked above") {
                out.push(rewrite(q, col));
            }
        }
        expanded = true;
    }
    s.target_list = out;
    Ok(changed | expanded)
}
