//! Where a set-returning function may be called: PostgreSQL's
//! `check_srf_call_placement` and the CASE / COALESCE checks of
//! `transformCaseExpr` / `transformCoalesceExpr`, applied as the statement
//! is analysed.
//!
//! An SRF runs in a select list (one ProjectSet per level) or as a FROM item.
//! Anywhere else PostgreSQL refuses it by where it is, and so does this
//! server -- before, `coalesce(unnest(a), 0)` and `count(generate_series(1,
//! 3))` answered rows where PostgreSQL 15 raises 0A000. A subquery is a
//! level of its own: `where x in (select generate_series(1, 2))` is allowed.
//! Measured on PostgreSQL 15.19 (`pg_corpora/srf_placement.sql`).

use super::*;

/// A call that returns a set: a built-in every overload of which does, or a
/// user function declared `RETURNS SETOF` / `TABLE`.
fn is_srf(f: &pg_query::protobuf::FuncCall) -> bool {
    let Some(name) = func_name(f) else {
        return false;
    };
    if correlated::user_function_named(&name) {
        return correlated::user_function_for(&name, &f.args).is_some_and(|u| u.returns_set);
    }
    f.funcname.len() <= 2 && funcsig::always_returns_set(&name)
}

/// The SRF calls inside `node` at its own query level: none inside a
/// subquery, which is checked as a statement of its own.
fn srfs_in(node: &N) -> Vec<&pg_query::protobuf::FuncCall> {
    let nested: Vec<*const pg_query::protobuf::FuncCall> = node
        .nodes()
        .into_iter()
        .filter_map(|(n, _, _, _)| match n {
            pg_query::NodeRef::SubLink(sl) => sl.subselect.as_deref(),
            _ => None,
        })
        .flat_map(|sub| {
            sub.node
                .as_ref()
                .map(|n| n.nodes())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(n, _, _, _)| match n {
                    pg_query::NodeRef::FuncCall(f) => Some(std::ptr::from_ref(f)),
                    _ => None,
                })
        })
        .collect();
    let mut out: Vec<&pg_query::protobuf::FuncCall> = node
        .nodes()
        .into_iter()
        .filter_map(|(n, _, _, _)| match n {
            pg_query::NodeRef::FuncCall(f) => Some(f),
            _ => None,
        })
        .filter(|f| is_srf(f) && !nested.contains(&std::ptr::from_ref(*f)))
        .collect();
    out.sort_by_key(|f| f.location);
    out
}

fn srfs_in_node(n: Option<&pg_query::protobuf::Node>) -> Vec<&pg_query::protobuf::FuncCall> {
    n.and_then(|n| n.node.as_ref())
        .map(srfs_in)
        .unwrap_or_default()
}

const LATERAL_HINT: &str =
    "You might be able to move the set-returning function into a LATERAL FROM item.";

fn refuse(f: &pg_query::protobuf::FuncCall, msg: String, hint: bool) -> Error {
    set_error_location(f.location);
    Error::Sqlstate(
        "0A000",
        if hint {
            format!("{msg}\nHint: {LATERAL_HINT}")
        } else {
            msg
        },
    )
}

fn not_allowed_in(srfs: &[&pg_query::protobuf::FuncCall], what: &str) -> Result<()> {
    match srfs.first() {
        Some(f) => Err(refuse(
            f,
            format!("set-returning functions are not allowed in {what}"),
            false,
        )),
        None => Ok(()),
    }
}

/// The location PostgreSQL's `exprLocation` gives an expression: its
/// leftmost token for an operator expression.
fn leftmost(n: &pg_query::protobuf::Node) -> i32 {
    match n.node.as_ref() {
        Some(N::AExpr(e)) => e.lexpr.as_deref().map_or(e.location, leftmost),
        Some(N::BoolExpr(b)) => b.args.first().map_or(b.location, leftmost),
        Some(N::TypeCast(tc)) => tc.arg.as_deref().map_or(tc.location, leftmost),
        Some(N::FuncCall(f)) => f.location,
        Some(N::ColumnRef(c)) => c.location,
        Some(N::AConst(c)) => c.location,
        _ => -1,
    }
}

/// Refuse every SRF `stmt` calls where PostgreSQL 15 does.
pub(crate) fn check(stmt: &N) -> Result<()> {
    // Expression contexts, at any level: the SRF must not be inside them.
    for (n, _, _, _) in stmt.nodes() {
        match n {
            pg_query::NodeRef::CaseExpr(c) => {
                // A WHEN condition returning a set fails as it is coerced to
                // boolean, before the CASE as a whole is looked at.
                for w in &c.args {
                    if let Some(N::CaseWhen(cw)) = w.node.as_ref() {
                        if let Some(expr) = cw.expr.as_deref() {
                            if !srfs_in_node(Some(expr)).is_empty() {
                                set_error_location(leftmost(expr));
                                return Err(Error::Sqlstate(
                                    "42804",
                                    "argument of CASE/WHEN must not return a set".into(),
                                ));
                            }
                        }
                    }
                }
                // The operand of a simple CASE (`case x when ...`) too, which
                // the node walk does not reach.
                let wrapped = N::CaseExpr(Box::new(c.clone()));
                let mut found = srfs_in(&wrapped);
                found.extend(srfs_in_node(c.arg.as_deref()));
                found.sort_by_key(|f| f.location);
                if let Some(f) = found.last() {
                    return Err(refuse(
                        f,
                        "set-returning functions are not allowed in CASE".into(),
                        true,
                    ));
                }
            }
            pg_query::NodeRef::CoalesceExpr(c) => {
                let wrapped = N::CoalesceExpr(Box::new(c.clone()));
                if let Some(f) = srfs_in(&wrapped).last() {
                    return Err(refuse(
                        f,
                        "set-returning functions are not allowed in COALESCE".into(),
                        true,
                    ));
                }
            }
            pg_query::NodeRef::FuncCall(f) => {
                let inside: Vec<&pg_query::protobuf::FuncCall> = f
                    .args
                    .iter()
                    .chain(f.agg_order.iter())
                    .flat_map(|a| srfs_in_node(Some(a)))
                    .collect();
                if let Some(g) = inside.last() {
                    if f.over.is_some() {
                        return Err(refuse(
                            g,
                            "window function calls cannot contain set-returning function calls"
                                .into(),
                            true,
                        ));
                    }
                    if is_aggregate_call(f) {
                        return Err(refuse(
                            g,
                            "aggregate function calls cannot contain set-returning function calls"
                                .into(),
                            true,
                        ));
                    }
                }
                not_allowed_in(&srfs_in_node(f.agg_filter.as_deref()), "FILTER")?;
            }
            pg_query::NodeRef::JoinExpr(j) => {
                not_allowed_in(&srfs_in_node(j.quals.as_deref()), "JOIN conditions")?;
            }
            _ => {}
        }
    }
    // Clause contexts, per query level.
    for (n, _, _, _) in stmt.nodes() {
        match n {
            pg_query::NodeRef::SelectStmt(s) => {
                not_allowed_in(&srfs_in_node(s.where_clause.as_deref()), "WHERE")?;
                not_allowed_in(&srfs_in_node(s.having_clause.as_deref()), "HAVING")?;
                not_allowed_in(&srfs_in_node(s.limit_count.as_deref()), "LIMIT")?;
                not_allowed_in(&srfs_in_node(s.limit_offset.as_deref()), "OFFSET")?;
            }
            pg_query::NodeRef::UpdateStmt(u) => {
                let set: Vec<_> = u
                    .target_list
                    .iter()
                    .flat_map(|t| srfs_in_node(Some(t)))
                    .collect();
                not_allowed_in(&set, "UPDATE")?;
                not_allowed_in(&srfs_in_node(u.where_clause.as_deref()), "WHERE")?;
            }
            pg_query::NodeRef::DeleteStmt(d) => {
                not_allowed_in(&srfs_in_node(d.where_clause.as_deref()), "WHERE")?;
            }
            _ => {}
        }
    }
    // A VALUES list, except the one row of an INSERT's, which is its
    // target list (`insert into t values (generate_series(1, 2))` inserts
    // two rows).
    let insert_row: Option<*const pg_query::protobuf::SelectStmt> = match stmt {
        N::InsertStmt(i) => match i.select_stmt.as_deref().and_then(|n| n.node.as_ref()) {
            Some(N::SelectStmt(s)) if s.values_lists.len() == 1 => Some(std::ptr::from_ref(&**s)),
            _ => None,
        },
        _ => None,
    };
    for (n, _, _, _) in stmt.nodes() {
        if let pg_query::NodeRef::SelectStmt(s) = n {
            let here = std::ptr::from_ref(s);
            if s.values_lists.is_empty() || insert_row == Some(here) {
                continue;
            }
            let srfs: Vec<_> = s
                .values_lists
                .iter()
                .flat_map(|r| srfs_in_node(Some(r)))
                .collect();
            not_allowed_in(&srfs, "VALUES")?;
        }
    }
    Ok(())
}
