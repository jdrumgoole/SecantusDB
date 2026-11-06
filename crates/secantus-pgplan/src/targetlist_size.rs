//! The limits PostgreSQL puts on a relation's width (RP-387).
//!
//! A table holds at most `MaxHeapAttributeNumber` (1600) columns, and a
//! query level's target list at most `MaxTupleAttributeNumber` (1664)
//! entries, both refused with 54011 (`too_many_columns`). PostgreSQL counts
//! the target list once parse analysis has built it: a `*` stands for every
//! column it expands to, and an ORDER BY or GROUP BY expression the list does
//! not already hold adds a hidden (resjunk) entry. A VALUES list counts its
//! row's width the same way.
//!
//! Checked on the statement as written, before the planner's rewrites: they
//! walk the list, and some of them (the DISTINCT and grouped-aggregate
//! dedups) cost the square of its length.

use std::collections::HashSet;

use pg_query::protobuf::{self, Node, SelectStmt};
use pg_query::{NodeEnum as N, NodeRef};
use secantus_pgcatalog::TableDef;

use crate::{node_print, Error, Result};

/// `MaxHeapAttributeNumber`: the most columns a table can have.
pub const MAX_HEAP_ATTRIBUTE_NUMBER: i32 = 1600;
/// `MaxTupleAttributeNumber`: the most entries a target list can have.
pub const MAX_TUPLE_ATTRIBUTE_NUMBER: usize = 1664;

fn too_many_targets() -> Error {
    Error::Sqlstate(
        "54011",
        format!("target lists can have at most {MAX_TUPLE_ATTRIBUTE_NUMBER} entries"),
    )
}

/// Refuse a statement any of whose query levels has a target list over the
/// limit.
pub(crate) fn check(node: &N, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Result<()> {
    if let N::SelectStmt(s) = node {
        check_select(s, lookup)?;
    }
    for (n, _, _, _) in node.nodes() {
        if let NodeRef::SelectStmt(s) = n {
            check_select(s, lookup)?;
        }
    }
    Ok(())
}

fn check_select(s: &SelectStmt, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Result<()> {
    // A set operation's arms are query levels of their own.
    if s.op != protobuf::SetOperation::SetopNone as i32 {
        return Ok(());
    }
    if let Some(row) = s.values_lists.first() {
        if let Some(N::List(l)) = row.node.as_ref() {
            if l.items.len() > MAX_TUPLE_ATTRIBUTE_NUMBER {
                return Err(too_many_targets());
            }
        }
        return Ok(());
    }
    let width = target_width(s, lookup);
    if width > MAX_TUPLE_ATTRIBUTE_NUMBER {
        return Err(too_many_targets());
    }
    if s.sort_clause.is_empty() && s.group_clause.is_empty() {
        return Ok(());
    }
    if width + s.sort_clause.len() + s.group_clause.len() <= MAX_TUPLE_ATTRIBUTE_NUMBER {
        return Ok(());
    }
    if width + resjunk(s) > MAX_TUPLE_ATTRIBUTE_NUMBER {
        return Err(too_many_targets());
    }
    Ok(())
}

/// The entries `transformTargetList` builds, a `*` expanded.
fn target_width(s: &SelectStmt, lookup: &dyn Fn(&str) -> Option<TableDef>) -> usize {
    s.target_list
        .iter()
        .map(|t| match t.node.as_ref() {
            Some(N::ResTarget(r)) => match star_qualifier(r) {
                Some(None) => s.from_clause.iter().map(|f| from_width(f, lookup)).sum(),
                Some(Some(q)) => s
                    .from_clause
                    .iter()
                    .find_map(|f| named_width(f, &q, lookup))
                    .unwrap_or(0),
                None => 1,
            },
            _ => 1,
        })
        .sum()
}

/// `Some(None)` for `*`, `Some(Some(t))` for `t.*`, `None` for an
/// expression.
fn star_qualifier(r: &protobuf::ResTarget) -> Option<Option<String>> {
    let Some(N::ColumnRef(c)) = r.val.as_ref().and_then(|v| v.node.as_ref()) else {
        return None;
    };
    if !matches!(
        c.fields.last().and_then(|f| f.node.as_ref()),
        Some(N::AStar(_))
    ) {
        return None;
    }
    let qualifier = c
        .fields
        .len()
        .checked_sub(2)
        .and_then(|i| match c.fields[i].node.as_ref() {
            Some(N::String(q)) => Some(q.sval.clone()),
            _ => None,
        });
    Some(qualifier)
}

/// The columns a FROM item contributes to an unqualified `*` -- 0 where
/// they are not known here (a function, a view the lookup does not hold).
fn from_width(f: &Node, lookup: &dyn Fn(&str) -> Option<TableDef>) -> usize {
    match f.node.as_ref() {
        Some(N::RangeVar(v)) => lookup(&v.relname).map_or(0, |d| d.columns.len()),
        Some(N::RangeSubselect(r)) => match r.subquery.as_ref().and_then(|q| q.node.as_ref()) {
            Some(N::SelectStmt(q)) => target_width(q, lookup),
            _ => 0,
        },
        Some(N::JoinExpr(j)) => {
            let side = |n: &Option<Box<Node>>| n.as_deref().map_or(0, |n| from_width(n, lookup));
            // `USING` and NATURAL merge each joined pair into one column.
            let merged = if j.is_natural {
                0
            } else {
                j.using_clause.len()
            };
            (side(&j.larg) + side(&j.rarg)).saturating_sub(merged)
        }
        _ => 0,
    }
}

/// The width of the FROM item called `name`, if it is one of these.
fn named_width(f: &Node, name: &str, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Option<usize> {
    match f.node.as_ref() {
        Some(N::RangeVar(v)) => {
            let called = v
                .alias
                .as_ref()
                .map_or(v.relname.as_str(), |a| a.aliasname.as_str());
            (called == name).then(|| from_width(f, lookup))
        }
        Some(N::RangeSubselect(r)) => r
            .alias
            .as_ref()
            .filter(|a| a.aliasname == name)
            .map(|_| from_width(f, lookup)),
        Some(N::JoinExpr(j)) => [&j.larg, &j.rarg]
            .into_iter()
            .find_map(|n| n.as_deref().and_then(|n| named_width(n, name, lookup))),
        _ => None,
    }
}

/// The hidden entries ORDER BY and GROUP BY add: one per distinct
/// expression the target list does not already hold
/// (`findTargetlistEntrySQL99` / `SQL92`). An ordinal or an output name
/// refers to an entry; a bare column is taken to be one of a `*`'s.
fn resjunk(s: &SelectStmt) -> usize {
    let mut names = HashSet::new();
    let mut held = HashSet::new();
    let mut has_star = false;
    for t in &s.target_list {
        if let Some(N::ResTarget(r)) = t.node.as_ref() {
            if star_qualifier(r).is_some() {
                has_star = true;
            }
            if !r.name.is_empty() {
                names.insert(r.name.clone());
            }
            if let Some(v) = r.val.as_deref() {
                if let Some(N::ColumnRef(c)) = v.node.as_ref() {
                    if let Some(N::String(last)) = c.fields.last().and_then(|f| f.node.as_ref()) {
                        names.insert(last.sval.clone());
                    }
                }
                held.insert(node_print(v));
            }
        }
    }
    let sorts = s.sort_clause.iter().filter_map(|n| match n.node.as_ref() {
        Some(N::SortBy(b)) => b.node.as_deref(),
        _ => None,
    });
    let mut added = 0;
    for e in sorts.chain(s.group_clause.iter()) {
        match e.node.as_ref() {
            Some(N::AConst(_)) | Some(N::GroupingSet(_)) | None => continue,
            Some(N::ColumnRef(c)) => {
                let bare = match c.fields.as_slice() {
                    [f] => match f.node.as_ref() {
                        Some(N::String(n)) => Some(&n.sval),
                        _ => None,
                    },
                    _ => None,
                };
                if has_star || bare.is_some_and(|n| names.contains(n)) {
                    continue;
                }
            }
            _ => {}
        }
        if held.insert(node_print(e)) {
            added += 1;
        }
    }
    added
}
