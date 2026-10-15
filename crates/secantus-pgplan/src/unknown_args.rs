//! An untyped string literal given to a built-in function takes the type of
//! the parameter of the overload PostgreSQL resolves the call to
//! (`func_select_candidate`'s unknown-argument rules): `abs('-1')` is
//! `abs(float8)` and answers the double 1, `abs('x')` is 22P02 for type
//! double precision at the literal. Without this the planner saw a string
//! and refused the call by its Rust value (`abs of String("x")`).
//!
//! Applied only to a call whose every argument is a constant, so the types
//! are all known here, before the statement is planned: a call over a
//! column is typed by the planner, which knows the column's type.
//! Measured against PostgreSQL 15.19 (`pg_corpora/unknown_args.sql`).

use super::*;

/// The type of a constant argument as overload resolution sees it: an
/// untyped string or NULL is `unknown`; `None` for anything not constant.
fn const_arg_type(n: &pg_query::protobuf::Node) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    match n.node.as_ref()? {
        N::AConst(c) if c.isnull => Some("unknown".into()),
        N::AConst(c) => Some(
            match c.val.as_ref()? {
                Val::Sval(_) => "unknown",
                Val::Ival(_) => "int4",
                Val::Fval(_) => "numeric",
                Val::Boolval(_) => "bool",
                _ => return None,
            }
            .into(),
        ),
        N::TypeCast(tc)
            if matches!(
                tc.arg.as_deref().and_then(|a| a.node.as_ref()),
                Some(N::AConst(_))
            ) =>
        {
            let t = type_name_of(tc.type_name.as_ref()?);
            (!t.contains('(')).then_some(t)
        }
        _ => None,
    }
}

/// An untyped string literal or NULL, by its location.
fn untyped_string(n: &pg_query::protobuf::Node) -> Option<i32> {
    match n.node.as_ref()? {
        N::AConst(c)
            if c.isnull || matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) =>
        {
            Some(c.location)
        }
        _ => None,
    }
}

/// The name and argument types of a constant built-in call with an untyped
/// argument, when the rules here apply to it. An aggregate is resolved the
/// same way (`sum('1')` is 42725) but only `check_ambiguous` judges one.
fn unknown_call(
    f: &pg_query::protobuf::FuncCall,
    aggregates: bool,
) -> Option<(String, Vec<String>)> {
    if f.agg_star || f.func_variadic || f.over.is_some() || f.funcname.len() > 2 {
        return None;
    }
    if !f.agg_order.is_empty() || f.agg_within_group || f.agg_filter.is_some() || f.agg_distinct {
        return None;
    }
    let name = func_name(f)?;
    if correlated::user_function_named(&name)
        || crate::user_agg::is_user_aggregate(&name)
        || (!aggregates && is_aggregate_call(f))
    {
        return None;
    }
    if !f.args.iter().any(|a| untyped_string(a).is_some()) {
        return None;
    }
    let types: Vec<String> = f.args.iter().map(const_arg_type).collect::<Option<_>>()?;
    Some((name, types))
}

/// The parameter types for `f`'s untyped literals, when they are to be cast.
fn resolved(f: &pg_query::protobuf::FuncCall) -> Option<Vec<String>> {
    let (name, types) = unknown_call(f, false)?;
    funcsig::resolved_args(&name, &types).or_else(|| funcsig::polymorphic_args(&name, &types))
}

/// PostgreSQL's 42725 for a constant call its unknown-argument rules leave
/// with several overloads (`trunc('2.7')`: numeric or float8), at the call.
pub(crate) fn check_ambiguous(node: &N) -> Result<()> {
    for (n, _, _, _) in node.nodes() {
        let pg_query::NodeRef::FuncCall(f) = n else {
            continue;
        };
        let Some((name, types)) = unknown_call(f, true) else {
            continue;
        };
        // Only a call of untyped arguments alone: beside a typed one the
        // rules here do not yet follow every step of PostgreSQL's (a `date`
        // reaching `overlaps(timestamptz, ...)` through its implicit cast).
        if funcsig::unknown_polymorphic(&name, &types) {
            return Err(Error::Sqlstate(
                "42804",
                "could not determine polymorphic type because input has type unknown".into(),
            ));
        }
        if types.iter().all(|t| t == "unknown") && funcsig::unknown_ambiguous(&name, &types) {
            // Named as written, schema and all: the grammar's own calls are
            // `pg_catalog.extract`.
            let written: Vec<&str> = f
                .funcname
                .iter()
                .filter_map(|n| match n.node.as_ref() {
                    Some(N::String(s)) => Some(s.sval.as_str()),
                    _ => None,
                })
                .collect();
            let name = if written.len() == 2 {
                written.join(".")
            } else {
                name
            };
            set_error_location(f.location);
            return Err(Error::Sqlstate(
                "42725",
                format!(
                    "function {name}({}) is not unique\nHint: Could not choose a best candidate \
                     function. You might need to add explicit type casts.",
                    types
                        .iter()
                        .map(|t| display_type(t))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    }
    Ok(())
}

/// Parameter types whose functions read an untyped literal themselves, in the
/// session's context, which a cast made while parsing would get wrong: a
/// date or time (read in the session zone when the call runs -- a range
/// constructor given `'2020-01-01 00:00+00'` shifted by the offset), and an
/// object name (`nextval('public.s')` resolves it at the call).
fn own_reading(ty: &str) -> bool {
    let base = ty.trim_end_matches("[]");
    crate::datetime::is_datetime(base)
        || matches!(base, "interval" | "timetz" | "time" | "date")
        || base.starts_with("reg")
}

/// Cast each untyped string argument of a constant built-in call to the
/// type of the parameter it is resolved to.
pub(crate) fn rewrite(node: &mut N) {
    let wanted = node
        .nodes()
        .iter()
        .any(|(n, _, _, _)| matches!(n, pg_query::NodeRef::FuncCall(f) if resolved(f).is_some()));
    if !wanted {
        return;
    }
    // SAFETY: the pointers `nodes_mut` hands out point into `node`, which
    // outlives the loop; each call's arguments are changed in place.
    unsafe {
        for (n, _, _) in node.nodes_mut() {
            let pg_query::NodeMut::FuncCall(f) = n else {
                continue;
            };
            let f = &mut *f;
            let Some(params) = resolved(f) else {
                continue;
            };
            for (arg, ty) in f.args.iter_mut().zip(params) {
                let Some(location) = untyped_string(arg) else {
                    continue;
                };
                // A text parameter takes the literal as it is; a name the
                // catalog quotes (`"char"`) is not one to spell back.
                if ty == "text" || ty.contains('"') || ty.starts_with("any") || own_reading(&ty) {
                    continue;
                }
                let literal = std::mem::take(arg);
                *arg = pg_query::protobuf::Node {
                    node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
                        arg: Some(Box::new(literal)),
                        type_name: Some(type_name_node(&ty)),
                        location,
                    }))),
                };
            }
        }
    }
}
