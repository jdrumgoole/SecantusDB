//! `FROM f(...) WITH ORDINALITY` and `FROM ROWS FROM (f(...), g(...))`, as
//! the subqueries they mean.
//!
//! * `WITH ORDINALITY` adds a `bigint` column numbering the function's rows
//!   from 1 in the order it produced them: `row_number() OVER ()` over the
//!   function as a FROM item.
//! * `ROWS FROM` runs several functions side by side, the shorter ones padded
//!   with NULL: exactly what a multi-argument `unnest` does to arrays, so each
//!   function becomes `ARRAY(SELECT f(...))` and the arrays are unnested
//!   together.
//!
//! Before this rewrite both forms reached the single-function path, which
//! silently kept the FIRST function and dropped the ordinality column -- a
//! wrong answer rather than an error. Measured against PostgreSQL 14.

use super::*;

/// The select with every such FROM item rewritten, or `None` if it has none.
pub(crate) fn rewrite(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let mut out = s.clone();
    let mut changed = false;
    for item in &mut out.from_clause {
        changed |= rewrite_item(item)?;
    }
    Ok(changed.then_some(out))
}

fn rewrite_item(item: &mut pg_query::protobuf::Node) -> Result<bool> {
    match item.node.as_mut() {
        Some(N::JoinExpr(j)) => {
            let mut changed = false;
            if let Some(l) = j.larg.as_deref_mut() {
                changed |= rewrite_item(l)?;
            }
            if let Some(r) = j.rarg.as_deref_mut() {
                changed |= rewrite_item(r)?;
            }
            Ok(changed)
        }
        Some(N::RangeFunction(rf)) if rf.ordinality || rf.functions.len() > 1 => {
            let rewritten = as_subselect(rf)?;
            item.node = Some(rewritten);
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) fn as_subselect(rf: &pg_query::protobuf::RangeFunction) -> Result<N> {
    // Each entry is a list of (call, column definition list).
    let mut calls = Vec::new();
    let mut names = Vec::new();
    for f in &rf.functions {
        let Some(N::List(l)) = f.node.as_ref() else {
            return Err(Error::Unsupported("this ROWS FROM item".into()));
        };
        let has_coldefs = l
            .items
            .get(1)
            .is_some_and(|c| matches!(c.node.as_ref(), Some(N::List(cl)) if !cl.items.is_empty()));
        if has_coldefs || !rf.coldeflist.is_empty() {
            return Err(Error::Unsupported(
                "a column definition list with ROWS FROM or WITH ORDINALITY".into(),
            ));
        }
        let Some(call) = l.items.first() else {
            return Err(Error::Unsupported("this ROWS FROM item".into()));
        };
        let Some(N::FuncCall(fc)) = call.node.as_ref() else {
            return Err(Error::Unsupported("this ROWS FROM item".into()));
        };
        names.push(func_name(fc).unwrap_or_else(|| "unnest".into()));
        calls.push(deparse_expr(call)?);
    }
    let rows = if let [only] = calls.as_slice() {
        format!("SELECT * FROM {only}")
    } else {
        let arrays: Vec<String> = calls.iter().map(|c| format!("ARRAY(SELECT {c})")).collect();
        format!("SELECT * FROM unnest({})", arrays.join(", "))
    };
    let sql = if rf.ordinality {
        format!("SELECT s.*, row_number() OVER () AS ordinality FROM ({rows}) AS s")
    } else {
        rows
    };
    let N::SelectStmt(body) = parse_one(&sql)? else {
        return Err(Error::Internal("ROWS FROM rewrite".into()));
    };
    // Without an alias the relation is named for the (first) function, and
    // ROWS FROM's columns for theirs -- PostgreSQL's defaults.
    let mut alias = rf.alias.clone().unwrap_or_else(|| pg_query::protobuf::Alias {
        aliasname: names.first().cloned().unwrap_or_default(),
        colnames: Vec::new(),
    });
    if alias.colnames.is_empty() && calls.len() > 1 {
        let string = |v: &str| pg_query::protobuf::Node {
            node: Some(N::String(pg_query::protobuf::String { sval: v.to_string() })),
        };
        alias.colnames = names.iter().map(|n| string(n)).collect();
        if rf.ordinality {
            alias.colnames.push(string("ordinality"));
        }
    }
    Ok(N::RangeSubselect(Box::new(pg_query::protobuf::RangeSubselect {
        lateral: rf.lateral,
        subquery: Some(Box::new(pg_query::protobuf::Node {
            node: Some(N::SelectStmt(body)),
        })),
        alias: Some(alias),
    })))
}
