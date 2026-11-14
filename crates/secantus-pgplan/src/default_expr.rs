//! The parse analysis of a DEFAULT (RP-881): PostgreSQL's `cookDefault`,
//! which runs `transformExpr` over the expression with
//! `EXPR_KIND_COLUMN_DEFAULT` when the default is declared -- by CREATE
//! TABLE, ALTER TABLE ... ADD COLUMN / SET DEFAULT and CREATE DOMAIN -- not
//! when an INSERT first uses it.
//!
//! What the analysis refuses, at the token it meets first (measured on
//! PostgreSQL 15.19):
//!
//! - a column reference, bound or not: 0A000 `cannot use column reference
//!   in DEFAULT expression` (`transformColumnRef`);
//! - a subquery of any kind: 0A000 `cannot use subquery in DEFAULT
//!   expression` (`transformSubLink`), before its body is looked at;
//! - a call no function takes: 42883, or 3F000 for a schema that does not
//!   exist, once its arguments are analysed (`ParseFuncOrColumn`);
//! - an aggregate: 42803 (`check_agglevels_and_constraints`), a window
//!   function: 42P20 (`transformWindowFuncCall`), and a set-returning
//!   function: 0A000 (`check_srf_call_placement`), each `... are not
//!   allowed in DEFAULT expressions`; an aggregate's or window's clause on
//!   a plain function is 42809 (`not_an_aggregate_function`).
//!
//! A call's arguments are analysed before the call, and an operator's left
//! operand before its right, so the first of these in that order is the
//! one reported. Nothing is evaluated: a call is resolved with user code
//! and sequence draws switched off.

use super::*;

use pg_query::protobuf::{FuncCall, Node};

/// `ParseExprKindName(EXPR_KIND_COLUMN_DEFAULT)`.
const KIND: &str = "DEFAULT expressions";

/// PostgreSQL 15's window functions that are not aggregates (`prokind`
/// `w`).
const WINDOW_FUNCTIONS: &[&str] = &[
    "row_number",
    "rank",
    "dense_rank",
    "percent_rank",
    "cume_dist",
    "ntile",
    "lag",
    "lead",
    "first_value",
    "last_value",
    "nth_value",
];

/// Analyse a DEFAULT as `cookDefault` does, failing at its first error.
pub(crate) fn analyse(raw: &Node) -> Result<()> {
    walk_expr(&mut raw.clone(), &mut visit)
}

/// [`analyse`] for a statement whose errors PostgreSQL reports with no
/// position: ALTER TABLE and CREATE DOMAIN analyse the default with no
/// source text in the parse state.
pub(crate) fn analyse_unpositioned(raw: &Node) -> Result<()> {
    analyse(raw).inspect_err(|_| set_error_unpositioned())
}

/// One node of the walk, which reaches nodes in `transformExpr`'s order
/// (an operator's operands left to right). A call is analysed here, its
/// arguments first, and then emptied so the walk does not descend again.
fn visit(n: &mut Node) -> Result<()> {
    match n.node.as_mut() {
        Some(N::ColumnRef(c)) => {
            set_error_location(c.location);
            Err(Error::Sqlstate(
                "0A000",
                "cannot use column reference in DEFAULT expression".into(),
            ))
        }
        Some(N::SubLink(s)) => {
            set_error_location(s.location);
            Err(Error::Sqlstate(
                "0A000",
                "cannot use subquery in DEFAULT expression".into(),
            ))
        }
        Some(N::FuncCall(f)) => {
            call(f)?;
            f.args.clear();
            f.agg_order.clear();
            f.agg_filter = None;
            f.over = None;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// `transformFuncCall` then `ParseFuncOrColumn` for one call.
fn call(f: &mut FuncCall) -> Result<()> {
    // The arguments, and a WITHIN GROUP's ordering, which is analysed as
    // more arguments.
    for a in f.args.iter_mut() {
        walk_expr(a, &mut visit)?;
    }
    if f.agg_within_group {
        for a in f.agg_order.iter_mut() {
            walk_expr(a, &mut visit)?;
        }
    }
    if let Some(missing) = refused_call_name(f) {
        return Err(missing.error());
    }
    let builtin = match f.funcname.as_slice() {
        [_] => true,
        [schema, _] => matches!(schema.node.as_ref(), Some(N::String(s)) if s.sval == "pg_catalog"),
        _ => false,
    };
    let name = func_name(f).unwrap_or_default();
    if builtin && is_builtin_aggregate_or_window(&name) && !correlated::user_function_named(&name) {
        set_error_location(f.location);
        if WINDOW_FUNCTIONS.contains(&name.as_str()) && f.over.is_none() {
            return Err(Error::Sqlstate(
                "42809",
                format!("window function {name} requires an OVER clause"),
            ));
        }
        if f.over.is_some() {
            return Err(Error::Sqlstate(
                "42P20",
                format!("window functions are not allowed in {KIND}"),
            ));
        }
        // `transformAggregateCall` analyses the ordering and the FILTER
        // before it checks where the aggregate is.
        for a in f.agg_order.iter_mut() {
            walk_expr(a, &mut visit)?;
        }
        if let Some(filter) = f.agg_filter.as_deref_mut() {
            walk_expr(filter, &mut visit)?;
        }
        set_error_location(f.location);
        return Err(Error::Grouping(format!(
            "aggregate functions are not allowed in {KIND}"
        )));
    }
    resolve(f)?;
    if f.agg_star
        || f.agg_distinct
        || f.agg_within_group
        || !f.agg_order.is_empty()
        || f.agg_filter.is_some()
        || f.over.is_some()
    {
        return Err(not_an_aggregate_function(f));
    }
    if srf_place::is_srf(f) {
        set_error_location(f.location);
        return Err(Error::Sqlstate(
            "0A000",
            format!("set-returning functions are not allowed in {KIND}"),
        ));
    }
    Ok(())
}

/// Look up a plain call, its arguments known to analyse: an error parse
/// analysis raises (a function found nowhere, a type error) stands, at the
/// call when it carries no position of its own; a refusal of something
/// this server does not fold, or a data error from folding, is not
/// analysis and is left to the default's evaluation.
fn resolve(f: &FuncCall) -> Result<()> {
    let plain = Node {
        node: Some(N::FuncCall(Box::new(FuncCall {
            over: None,
            agg_star: false,
            agg_distinct: false,
            agg_within_group: false,
            agg_order: Vec::new(),
            agg_filter: None,
            ..f.clone()
        }))),
    };
    take_error_location();
    let folded =
        correlated::without_calls(|| correlated::without_side_effects(|| const_value(&plain, &[])));
    let e = match folded {
        Ok(_) => return Ok(()),
        Err(Error::Unsupported(m)) if fromless::names_missing_function(&m) => Error::Unsupported(m),
        Err(e) if !["0A", "21", "22"].contains(&&e.sqlstate()[..2]) => e,
        Err(_) => {
            take_error_location();
            return Ok(());
        }
    };
    if peek_error_location().is_none() {
        set_error_location(f.location);
    }
    Err(e)
}

/// What `cookDefault` leaves of a DEFAULT whose folding raised `e` (RP-886):
/// the analysis coerces each string literal to the type it meets, which runs
/// that type's input function, and evaluates nothing else. So a literal's
/// input error stands, at the declaration; any other error a constant
/// raises (`1/0`, an overflow, a domain's CHECK) is the error of the first
/// INSERT that uses the default, and `Ok` says to keep it as its
/// expression. An error of the analysis itself (any class but data and
/// integrity) stands as well.
pub(crate) fn folding_error_stands(raw: &Node, pg_type: &str, e: Error) -> Result<()> {
    if !["22", "23"].contains(&&e.sqlstate()[..2]) {
        return Err(e);
    }
    // The whole default a literal: coerced to the column's type.
    if let Some(s) = string_literal(raw) {
        input(s, pg_type)?;
        take_error_location();
        return Ok(());
    }
    // A literal cast to a type is read by that type's input function
    // (`'x'::int`); one an operator or a call types is coerced by the same
    // function, to a type found only by resolving the operator, so it is
    // known by the value the error quotes (`'x' + 1` is 22P02 for "x").
    let message = e.to_string();
    let mut quoted_literal = false;
    walk_expr(&mut raw.clone(), &mut |n| {
        if let Some(N::TypeCast(tc)) = n.node.as_mut() {
            if let (Some(s), Some(tn)) = (tc.arg.as_deref().and_then(string_literal), &tc.type_name)
            {
                input(s, &type_name_of(tn))?;
                tc.arg = None;
            }
        } else if let Some(s) = string_literal(n) {
            quoted_literal |= message.contains(&format!("\"{s}\""));
        }
        Ok(())
    })?;
    if quoted_literal {
        return Err(e);
    }
    take_error_location();
    Ok(())
}

/// The value of a string constant (an `A_Const` of type unknown).
fn string_literal(n: &Node) -> Option<&str> {
    match n.node.as_ref()? {
        N::AConst(c) => match c.val.as_ref()? {
            pg_query::protobuf::a_const::Val::Sval(s) => Some(&s.sval),
            _ => None,
        },
        _ => None,
    }
}

/// A string literal through the input function of `ty` (`stringTypeDatum`):
/// a domain's base type's, since its CHECK is `CoerceToDomain`'s, which runs
/// with the INSERT.
fn input(s: &str, ty: &str) -> Result<Bson> {
    let base = domains::base_type_and_typmod(ty).map_or_else(|| ty.to_string(), |(b, _, _)| b);
    cast_value(Bson::String(s.to_string()), &base)
}
