//! A column functionally dependent on the GROUP BY (RP-280).
//!
//! PostgreSQL accepts an ungrouped column of a table whose primary key is
//! grouped by in full (`check_ungrouped_columns_walker` ->
//! `check_functional_grouping`): every row of a group has the same key, so
//! the same value in each of the table's columns. Only a primary key counts
//! -- not a unique constraint, nor a deferrable key -- and only plain
//! columns of the table, as Vars, in a GROUP BY without grouping sets.
//!
//! The planner checks grouping on the raw statement, so the dependency is
//! made explicit there: each such column the query names is added to the
//! GROUP BY. That leaves the groups exactly as they were (the key already
//! fixes the column) and lets every grouping check accept the column.

use crate::{column_ref_name, relation_name, walk_column_refs, Result, TableDef, N};
use pg_query::protobuf as pb;

/// A FROM relation: its qualifier (alias or name) and definition.
struct Rel {
    qualifier: String,
    def: TableDef,
}

fn from_relations(
    items: &[pb::Node],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<Rel>,
) {
    for item in items {
        match item.node.as_ref() {
            Some(N::RangeVar(rv)) => {
                if let Some(def) = lookup(&relation_name(rv)) {
                    let qualifier = rv
                        .alias
                        .as_ref()
                        .map_or_else(|| rv.relname.clone(), |a| a.aliasname.clone());
                    out.push(Rel { qualifier, def });
                }
            }
            Some(N::JoinExpr(j)) => {
                // A join with an alias hides its members' names.
                if j.alias.is_some() {
                    continue;
                }
                let sides: Vec<pb::Node> = [j.larg.as_deref(), j.rarg.as_deref()]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                from_relations(&sides, lookup, out);
            }
            _ => {}
        }
    }
}

fn names(c: &pb::ColumnRef) -> Option<Vec<String>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// The relation (index into `rels`) and column a column reference names,
/// when it names a plain column of exactly one of them.
fn resolve(c: &pb::ColumnRef, rels: &[Rel]) -> Option<(usize, String)> {
    let parts = names(c)?;
    let has = |r: &Rel, col: &str| r.def.columns.iter().any(|x| x.name == col);
    match parts.as_slice() {
        [col] => {
            let mut found = rels.iter().enumerate().filter(|(_, r)| has(r, col));
            let (i, _) = found.next()?;
            if found.next().is_some() {
                return None;
            }
            Some((i, col.clone()))
        }
        [q, col] => {
            let (i, r) = rels.iter().enumerate().find(|(_, r)| &r.qualifier == q)?;
            has(r, col).then(|| (i, col.clone()))
        }
        _ => None,
    }
}

/// The statement with each functionally dependent column the query names
/// added to its GROUP BY, or `None` when nothing changes.
pub(crate) fn rewrite(
    s: &pb::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<Option<pb::SelectStmt>> {
    if s.group_clause.is_empty() || s.group_distinct {
        return Ok(None);
    }
    let mut rels = Vec::new();
    from_relations(&s.from_clause, lookup, &mut rels);
    if rels.is_empty() {
        return Ok(None);
    }
    // The grouped plain columns; any grouping set leaves the check to
    // PostgreSQL's common-columns rule, which this does not model.
    let mut grouped: Vec<(usize, String)> = Vec::new();
    for g in &s.group_clause {
        match g.node.as_ref() {
            Some(N::ColumnRef(c)) => {
                if let Some(r) = resolve(c, &rels) {
                    grouped.push(r);
                }
            }
            Some(N::GroupingSet(_)) => return Ok(None),
            _ => {}
        }
    }
    // The relations whose whole primary key is grouped.
    let dependent: Vec<usize> = (0..rels.len())
        .filter(|&i| {
            let pk: Vec<&str> = rels[i]
                .def
                .columns
                .iter()
                .filter(|c| c.pk)
                .map(|c| c.name.as_str())
                .collect();
            !pk.is_empty()
                && pk
                    .iter()
                    .all(|k| grouped.iter().any(|(j, col)| *j == i && col == k))
        })
        .collect();
    if dependent.is_empty() {
        return Ok(None);
    }
    // The columns of those relations the query names and does not group.
    let mut add: Vec<(usize, String)> = Vec::new();
    let mut visit = |n: &pb::Node| -> Result<()> {
        let mut n = n.clone();
        walk_column_refs(&mut n, &mut |_, c| {
            if let Some((i, col)) = resolve(c, &rels) {
                if dependent.contains(&i)
                    && !grouped.contains(&(i, col.clone()))
                    && !add.contains(&(i, col.clone()))
                {
                    add.push((i, col));
                }
            }
            Ok(())
        })
    };
    for t in &s.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_ref() {
            if let Some(v) = rt.val.as_deref() {
                visit(v)?;
            }
        }
    }
    if let Some(h) = s.having_clause.as_deref() {
        visit(h)?;
    }
    for o in &s.sort_clause {
        if let Some(N::SortBy(sb)) = o.node.as_ref() {
            if let Some(n) = sb.node.as_deref() {
                // An output column's name is not a table column here.
                if let Some(N::ColumnRef(c)) = n.node.as_ref() {
                    if let Some(name) = column_ref_name(c) {
                        if c.fields.len() == 1
                            && s.target_list.iter().any(|t| {
                                matches!(t.node.as_ref(), Some(N::ResTarget(rt)) if rt.name == name)
                            })
                        {
                            continue;
                        }
                    }
                }
                visit(n)?;
            }
        }
    }
    if add.is_empty() {
        return Ok(None);
    }
    let mut out = s.clone();
    for (i, col) in add {
        out.group_clause.push(pb::Node {
            node: Some(N::ColumnRef(pb::ColumnRef {
                fields: [rels[i].qualifier.as_str(), col.as_str()]
                    .iter()
                    .map(|p| pb::Node {
                        node: Some(N::String(pb::String {
                            sval: (*p).to_string(),
                        })),
                    })
                    .collect(),
                location: -1,
            })),
        });
    }
    Ok(Some(out))
}
