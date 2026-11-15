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
    // `transformWithClause` refuses a name given twice before it analyses
    // any item, at the second (RP-504).
    let items: Vec<&pg_query::protobuf::CommonTableExpr> = with
        .ctes
        .iter()
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(c)) => Some(&**c),
            _ => None,
        })
        .collect();
    for (i, c) in items.iter().enumerate() {
        if let Some(dup) = items[i + 1..].iter().find(|d| d.ctename == c.ctename) {
            set_error_location(dup.location);
            return Err(Error::Sqlstate(
                "42712",
                format!(
                    "WITH query name \"{}\" specified more than once",
                    dup.ctename
                ),
            ));
        }
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
    // An item with a column alias list, or one naming an item written after
    // it, is analysed too: `analyzeCTETargetList` counts the aliases against
    // the item's columns (42P10), and `parserOpenTable` hints at a forward
    // reference (RP-504), neither of which the inlined statement reports.
    let forward = |i: usize| {
        names[i + 1..]
            .iter()
            .any(|later| reads(&with.ctes[i], later))
    };
    let Some(last) = (0..names.len())
        .rev()
        .find(|&i| !read(i) || !items[i].aliascolnames.is_empty() || forward(i))
    else {
        return Ok(());
    };
    let Ok(defs) = cte_defs(with) else {
        return Ok(());
    };
    for (i, (name, body, aliases)) in defs.iter().enumerate().take(last + 1) {
        take_error_location();
        let planned = planning_without_running(|| plan_select(body, lookup, params));
        // `transformTypeCast` reads an untyped literal through its type's
        // input function as the cast is analysed, so an unread item's
        // `'x'::int` is 22P02 -- before any error written after it.
        let at = match &planned {
            Err(_) => peek_error_location(),
            Ok(_) => None,
        };
        if let Some(e) = literal_input_error(body, params, at) {
            return Err(e);
        }
        match planned {
            Ok(_) => {
                if let Some(columns) = domain_scope::output_columns(body, lookup, &Vec::new()) {
                    if columns.len() < aliases.len() {
                        set_error_location(items[i].location);
                        return Err(Error::Sqlstate(
                            "42P10",
                            format!(
                                "WITH query \"{name}\" has {} columns available but {} columns specified",
                                columns.len(),
                                aliases.len()
                            ),
                        ));
                    }
                }
            }
            Err(Error::UndefinedTable(t)) if names[i + 1..].contains(&t.as_str()) => {
                return Err(Error::Sqlstate(
                    "42P01",
                    format!(
                        "relation \"{t}\" does not exist\nHint: Use WITH RECURSIVE, or re-order the WITH items to remove forward references."
                    ),
                ));
            }
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

/// The first untyped string literal under a cast in `body`, in the order
/// written and before `before` when that is given, whose type's input
/// function refuses it: that refusal, placed at the literal.
fn literal_input_error(
    body: &pg_query::protobuf::SelectStmt,
    params: &[Bson],
    before: Option<i32>,
) -> Option<Error> {
    let node = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(body.clone()))),
    };
    let mut casts: Vec<(i32, pg_query::protobuf::Node)> = node
        .node
        .as_ref()?
        .nodes()
        .into_iter()
        .filter_map(|(n, _, _, _)| match n {
            pg_query::NodeRef::TypeCast(tc) => {
                match tc.arg.as_deref().and_then(|a| a.node.as_ref()) {
                    Some(N::AConst(c))
                        if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
                            && c.location >= 0 =>
                    {
                        Some((
                            c.location,
                            pg_query::protobuf::Node {
                                node: Some(N::TypeCast(Box::new(tc.clone()))),
                            },
                        ))
                    }
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    casts.sort_by_key(|(at, _)| *at);
    for (at, cast) in casts {
        if before.is_some_and(|b| at >= b) {
            break;
        }
        take_error_location();
        match correlated::without_side_effects(|| const_value(&cast, params)) {
            Err(e) if e.sqlstate().starts_with("22") => {
                take_error_location();
                set_error_location(at);
                return Some(e);
            }
            _ => {
                take_error_location();
            }
        }
    }
    if let Some(b) = before {
        set_error_location(b);
    }
    None
}
