//! Column references in a SELECT with no FROM (RP-441).
//!
//! PostgreSQL's `transformColumnRef` looks a name up in the query's range
//! table, which a FROM-less select leaves empty, so every column reference
//! fails at its own token: a bare name is 42703 `column "x" does not
//! exist`, a qualified one 42P01 `missing FROM-clause entry for table
//! "t"` (`errorMissingRTE`), with no HINT since there is nothing to
//! suggest. Without this check the planner met the name only while
//! evaluating the expression around it, and answered `0A000 ColumnRef is
//! not supported yet`, a type error for the name read as an untyped value
//! (`abs(nosuch)` as `abs(text)`), or nothing at all in an ORDER BY,
//! GROUP BY or DISTINCT ON it ignored.

use super::*;

use pg_query::protobuf::{ColumnRef, Node, SelectStmt};

/// The first column reference of a FROM-less select, as PostgreSQL's
/// parse analysis reaches it.
///
/// The clauses go in `transformSelectStmt`'s order: the target list,
/// WHERE, HAVING, ORDER BY, GROUP BY, DISTINCT ON, OFFSET, LIMIT, and then
/// the window definitions (the WINDOW clause's, then each `OVER (...)` in
/// the order its function was met), which `transformWindowDefinitions`
/// analyses last. A bare name heading an ORDER BY, GROUP BY or DISTINCT ON
/// item may name an output column instead (`findTargetlistEntrySQL92`):
/// such an item is left to the planner. A subquery's body is its own
/// query and is not walked. A `VALUES` list is checked row by row
/// (`check_values_row`).
///
/// What is analysed before the reference fails first, as PostgreSQL
/// analyses a node's arguments before the node: an untyped string literal
/// under a cast is read by its type's input function as the cast is
/// analysed (`transformTypeCast`), so `select 'x'::int, 1 + nosuch` is
/// 22P02; and a call with no reference in its arguments is looked up as
/// it is analysed (`ParseFuncOrColumn`), so `select nosuchfn(1), nosuch`
/// is 42883. These are only weighed when a reference is there to report,
/// so a statement with none plans exactly as before.
pub(crate) fn check_column_refs(s: &SelectStmt, params: &[Bson]) -> Result<()> {
    if !s.from_clause.is_empty()
        || s.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || !s.values_lists.is_empty()
    {
        return Ok(());
    }
    match walk_clauses(s, None) {
        Ok(()) => Ok(()),
        Err(_) => {
            take_error_location();
            walk_clauses(s, Some(params))
        }
    }
}

/// The ordered walk: `ordered` holds the parameters to read a literal
/// cast or try a call with, or `None` to look at the references alone.
fn walk_clauses(s: &SelectStmt, ordered: Option<&[Bson]>) -> Result<()> {
    let outputs: Vec<String> = s
        .target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(rt)) if !rt.name.is_empty() => Some(rt.name.clone()),
            Some(N::ResTarget(rt)) => rt.val.as_deref().map(nodetostring::target::figure_colname),
            _ => None,
        })
        .collect();
    let names_output = |n: &Node| -> bool {
        let n = match n.node.as_ref() {
            Some(N::SortBy(sb)) => sb.node.as_deref(),
            _ => Some(n),
        };
        match n.and_then(|n| n.node.as_ref()) {
            Some(N::ColumnRef(c)) => match c.fields.as_slice() {
                [f] => matches!(f.node.as_ref(), Some(N::String(st)) if outputs.contains(&st.sval)),
                _ => false,
            },
            _ => false,
        }
    };
    let mut windows: Vec<Node> = s.window_clause.clone();
    {
        let mut visit = |n: &mut Node| visit_node(n, ordered, &mut windows);
        let walk = |n: &Node, visit: &mut dyn FnMut(&mut Node) -> Result<()>| {
            walk_expr(&mut n.clone(), visit)
        };
        for t in &s.target_list {
            walk(t, &mut visit)?;
        }
        for clause in [s.where_clause.as_deref(), s.having_clause.as_deref()]
            .into_iter()
            .flatten()
        {
            walk(clause, &mut visit)?;
        }
        for item in s
            .sort_clause
            .iter()
            .chain(&s.group_clause)
            .chain(&s.distinct_clause)
        {
            if !names_output(item) {
                walk(item, &mut visit)?;
            }
        }
        for clause in [s.limit_offset.as_deref(), s.limit_count.as_deref()]
            .into_iter()
            .flatten()
        {
            walk(clause, &mut visit)?;
        }
    }
    for w in &mut windows {
        walk_expr(w, &mut |n| match n.node.as_ref() {
            Some(N::ColumnRef(c)) => missing(c),
            _ => Ok(()),
        })?;
    }
    Ok(())
}

/// One node of the ordered walk: a column reference fails; with
/// `ordered`, a literal cast is read and a call with no reference in its
/// arguments is looked up first. A window is set aside in `windows`, as
/// it is analysed after the clauses.
fn visit_node(n: &mut Node, ordered: Option<&[Bson]>, windows: &mut Vec<Node>) -> Result<()> {
    match n.node.as_mut() {
        Some(N::FuncCall(f)) => {
            if let Some(over) = f.over.take() {
                windows.push(Node {
                    node: Some(N::WindowDef(over)),
                });
            }
            match ordered {
                Some(params) => missing_function(f, params),
                None => Ok(()),
            }
        }
        Some(N::ColumnRef(c)) => missing(c),
        Some(N::TypeCast(tc)) => match (ordered, tc.arg.as_deref()) {
            (Some(params), Some(arg)) if is_string_literal(arg) => {
                let cast = Node {
                    node: Some(N::TypeCast(tc.clone())),
                };
                match correlated::without_side_effects(|| const_value(&cast, params)) {
                    Err(e) if !matches!(e, Error::Unsupported(_)) => Err(e),
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

/// The first column reference of one row of a VALUES list with nothing
/// around it to bind it (RP-501). `transformValuesClause` transforms each
/// row's expressions in turn, before the row's length is compared and
/// before anything is evaluated, in a query level whose range table is
/// empty: so `values (1/0, nosuch)` is 42703 at `nosuch`, and what is
/// analysed before the reference fails first, as in a FROM-less select.
pub(crate) fn check_values_row(items: &[Node], params: &[Bson]) -> Result<()> {
    let walk = |ordered: Option<&[Bson]>| -> Result<()> {
        let mut windows = Vec::new();
        for item in items {
            walk_expr(&mut item.clone(), &mut |n| {
                visit_node(n, ordered, &mut windows)
            })?;
        }
        Ok(())
    };
    match walk(None) {
        Ok(()) => Ok(()),
        Err(_) => {
            take_error_location();
            walk(Some(params))
        }
    }
}

/// A call PostgreSQL has no function for, when nothing in its arguments
/// is a column reference that would fail first: the refusal the planner
/// gives such a call, which the server answers as 42883 (or 42809 for a
/// procedure). A call that exists, or that fails some other way, is left
/// to the planner.
fn missing_function(f: &pg_query::protobuf::FuncCall, params: &[Bson]) -> Result<()> {
    let call = Node {
        node: Some(N::FuncCall(Box::new(f.clone()))),
    };
    let mut has_reference = false;
    let _ = walk_expr(&mut call.clone(), &mut |n| {
        has_reference |= matches!(n.node.as_ref(), Some(N::ColumnRef(_)));
        Ok(())
    });
    if has_reference {
        return Ok(());
    }
    match correlated::without_side_effects(|| const_value(&call, params)) {
        Err(Error::Unsupported(m)) if names_missing_function(&m) => Err(Error::Unsupported(m)),
        _ => Ok(()),
    }
}

/// Whether a planner refusal `function name(types)` names a function that
/// neither PostgreSQL's catalog nor this database has.
pub(crate) fn names_missing_function(m: &str) -> bool {
    m.strip_prefix("function ")
        .and_then(|r| r.split_once('('))
        .is_some_and(|(name, _)| {
            !is_catalog_function(name) && !correlated::user_function_named(name)
        })
}

fn is_string_literal(n: &Node) -> bool {
    matches!(
        n.node.as_ref(),
        Some(N::AConst(c))
            if !c.isnull && matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
    )
}

/// The error `transformColumnRef` raises for a reference that an empty
/// range table cannot resolve, recorded at the reference's own token. A
/// bare `*` is `SELECT * with no tables specified`, answered where the
/// target list is planned.
fn missing(c: &ColumnRef) -> Result<()> {
    if c.fields.len() == 1 && matches!(c.fields[0].node.as_ref(), Some(N::AStar(_))) {
        return Ok(());
    }
    Err(values_refs::reference_error(c, None, &|_| None))
}
