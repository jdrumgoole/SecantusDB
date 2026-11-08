//! A WITH item no statement reads is still analysed (RP-501).
//!
//! PostgreSQL's `transformWithClause` parse-analyses every item of a WITH
//! in the order written, before the statement around it, whether or not
//! anything reads it: `with w as (select nosuch) select 1` is 42703 at
//! `nosuch`, and `with w as (select 1 from nosuch) select 1` 42P01. Only
//! the planner drops an unread item (`subquery_planner` plans no CTE whose
//! `cterefcount` is 0), so nothing in one is ever evaluated: `with w as
//! (select 1/0) select 1` answers 1.
//!
//! Here a non-recursive WITH is inlined as the FROM-subqueries it is
//! shorthand for, so an item no reference reaches was dropped unanalysed.
//! Each item up to the last unread one is planned before the statement,
//! without being run (`planning_without_running`), and an error parse
//! analysis would raise is the statement's. The items before an unread one
//! are planned too, read or not, so the first error in the WITH is the one
//! reported, as it is there; a WITH whose items are all read is left to the
//! inlined statement, which plans every one of them.

use super::*;

/// Analyse the items of `s`'s WITH as `transformWithClause` does, where an
/// item is not read by the statement or by an item after it.
pub(crate) fn analyse(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<()> {
    let Some(with) = s.with_clause.as_ref() else {
        return Ok(());
    };
    let mut rest = s.clone();
    rest.with_clause = None;
    let statement = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(rest))),
    };
    analyse_with(with, &statement, lookup, params)
}

/// The same for `with`, the WITH of `statement`, which is the statement
/// with that clause taken off: a SELECT's, or an UPDATE's or a DELETE's,
/// whose WITH `transformUpdateStmt` and `transformDeleteStmt` analyse
/// first, so `with w as (select nosuch) delete from t` deletes nothing.
pub(crate) fn analyse_with(
    with: &pg_query::protobuf::WithClause,
    statement: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<()> {
    // A recursive WITH is run before the statement is planned, and a
    // data-modifying item is materialised before this; what `cte_defs`
    // refuses is `inline_ctes`'s to report.
    if with.recursive {
        return Ok(());
    }
    let names: Vec<&str> = with
        .ctes
        .iter()
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(c)) => Some(c.ctename.as_str()),
            _ => None,
        })
        .collect();
    if names.len() != with.ctes.len() {
        return Ok(());
    }
    // Whether item `i` is read: an unqualified relation of its name anywhere
    // in the statement or in a later item. A nested WITH of the same name
    // counts too, so an item is only ever taken for unread when nothing
    // could read it.
    let reads = |node: &pg_query::protobuf::Node, name: &str| -> bool {
        node.node.as_ref().is_some_and(|node| {
            node.nodes().into_iter().any(|(n, _, _, _)| {
                matches!(n, pg_query::NodeRef::RangeVar(r)
                    if r.relname == name && r.schemaname.is_empty() && r.catalogname.is_empty())
            })
        })
    };
    let read = |i: usize| -> bool {
        reads(statement, names[i])
            || with.ctes[i + 1..]
                .iter()
                .any(|later| reads(later, names[i]))
    };
    let Some(last_unread) = (0..names.len()).rev().find(|&i| !read(i)) else {
        return Ok(());
    };
    let Ok(defs) = cte_defs(with) else {
        return Ok(());
    };
    for (_, body, _) in defs.iter().take(last_unread + 1) {
        let planned = planning_without_running(|| plan_select(body, lookup, params));
        match planned {
            Ok(_) => {}
            Err(e) if raised_by_analysis(&e) => return Err(e),
            // An error only running the item raises (a division by zero, a
            // value's input), or a shape this server refuses, which a
            // statement that never reads the item does not meet.
            Err(_) => {
                take_error_location();
            }
        }
    }
    Ok(())
}

/// An error parse analysis raises: an unknown or ambiguous name, a type
/// mismatch (class 42, but for a refused privilege, which is checked when
/// the statement runs), a missing schema, or a call no function answers.
fn raised_by_analysis(e: &Error) -> bool {
    match e {
        Error::Unsupported(m) => fromless::names_missing_function(m),
        e => {
            let code = e.sqlstate();
            (code.starts_with("42") && code != "42501") || code == "3F000"
        }
    }
}
