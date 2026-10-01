//! `CREATE AGGREGATE`: a user-defined aggregate is a state function folded
//! over the group's values from an initial condition, then an optional final
//! function over the state -- `advance_transition_function` and
//! `finalize_aggregate` from PostgreSQL's `nodeAgg.c`.
//!
//! The NULL rules are the part that is easy to get wrong, and they hinge on
//! the state function's STRICTness:
//!
//! - a STRICT state function is never called with a NULL input: the row is
//!   skipped;
//! - a STRICT state function with no initial condition takes the first
//!   non-NULL input AS the state, without being called;
//! - a non-strict one is called for every row, NULL state and NULL input
//!   included, which is how `initcond = '100'` over a `coalesce` body counts
//!   the NULL rows too.
//!
//! The catalog comes to the planner the way user functions do: the server
//! installs the list per statement.

use bson::Bson;

use crate::correlated::{call_user_function, user_functions, FnResult, UserFn};
use crate::{Error, Result};

/// One aggregate, as `CREATE AGGREGATE` declared it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UserAggregate {
    pub name: String,
    /// The declared argument types (`int4`, `anyelement`).
    pub arg_types: Vec<String>,
    pub sfunc: String,
    /// The state type, as declared (`anyarray` stays polymorphic here).
    pub stype: String,
    pub finalfunc: Option<String>,
    pub initcond: Option<String>,
}

thread_local! {
    static USER_AGGREGATES: std::cell::RefCell<Vec<UserAggregate>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the database's user aggregates for the statements that follow.
pub fn set_user_aggregates(aggs: Vec<UserAggregate>) {
    USER_AGGREGATES.with(|a| *a.borrow_mut() = aggs);
}

/// Whether some user aggregate has this name.
pub fn is_user_aggregate(name: &str) -> bool {
    USER_AGGREGATES.with(|a| a.borrow().iter().any(|u| u.name == name))
}

fn polymorphic(t: &str) -> bool {
    matches!(
        t,
        "anyelement" | "anycompatible" | "anyarray" | "anycompatiblearray" | "any"
    )
}

/// Whether two type names are one type (`integer` and `int4` are).
fn same_type(a: &str, b: &str) -> bool {
    let oid = |t: &str| crate::pgtypes::oid_of_name(t).map(|o| o.to_string());
    match (oid(a), oid(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// The aggregate `name(arg_type)` calls, or 42883 as PostgreSQL words it.
pub fn resolve(name: &str, arg_type: Option<&str>) -> Result<UserAggregate> {
    let found = USER_AGGREGATES.with(|a| {
        let aggs = a.borrow();
        let candidates: Vec<&UserAggregate> = aggs
            .iter()
            .filter(|u| u.name == name && u.arg_types.len() == 1)
            .collect();
        let exact = candidates
            .iter()
            .find(|u| arg_type.is_some_and(|t| same_type(&u.arg_types[0], t)));
        exact
            .or_else(|| candidates.iter().find(|u| polymorphic(&u.arg_types[0])))
            .or_else(|| {
                // A value of unknown type (an untyped literal) resolves when
                // only one candidate could take it.
                (arg_type.is_none() && candidates.len() == 1).then(|| &candidates[0])
            })
            .map(|u| (*u).clone())
    });
    found.ok_or_else(|| {
        Error::UndefinedFunction(format!(
            "function {name}({}) does not exist",
            crate::display_type(arg_type.unwrap_or("unknown"))
        ))
    })
}

/// The state type with its polymorphism resolved against the input type.
pub fn state_type(agg: &UserAggregate, source: Option<&str>) -> String {
    resolve_polymorphic(&agg.stype, source)
}

fn resolve_polymorphic(t: &str, source: Option<&str>) -> String {
    let source = source.unwrap_or("text");
    match t {
        "anyelement" | "anycompatible" | "any" => source.to_string(),
        "anyarray" | "anycompatiblearray" => {
            if source.ends_with("[]") {
                source.to_string()
            } else {
                format!("{source}[]")
            }
        }
        other => other.to_string(),
    }
}

/// The type the aggregate answers: the final function's, else the state's.
pub fn result_type(agg: &UserAggregate, source: Option<&str>) -> String {
    let state = state_type(agg, source);
    match &agg.finalfunc {
        Some(f) => match user_fn(f, std::slice::from_ref(&state)) {
            Some(u) => resolve_polymorphic(&u.return_type, Some(&state)),
            // PostgreSQL's own signature first: the static table answers
            // text for a name it does not know (`int4abs`).
            None => crate::funcsig::result_type(f, std::slice::from_ref(&state))
                .or_else(|| builtin_result_type(f))
                .unwrap_or(state),
        },
        None => state,
    }
}

/// The user function `name` taking these argument types.
fn user_fn(name: &str, arg_types: &[String]) -> Option<UserFn> {
    let fns = user_functions();
    let candidates: Vec<&UserFn> = fns
        .iter()
        .filter(|u| u.name == name && u.arg_types.len() == arg_types.len())
        .collect();
    candidates
        .iter()
        .find(|u| {
            u.arg_types
                .iter()
                .zip(arg_types)
                .all(|(p, a)| polymorphic(p) || same_type(p, a))
        })
        .map(|u| (*u).clone())
}

/// A built-in state or final function: what it computes over `(state,
/// value)`. The `*pl` / `*mi` / `*mul` / `*div` families are the operators;
/// `*larger` / `*smaller` are `max` / `min`.
fn builtin(name: &str) -> Option<Builtin> {
    const TYPES: [&str; 12] = [
        "int2", "int4", "int8", "int24", "int42", "int28", "int82", "int48", "int84", "float4",
        "float8", "numeric",
    ];
    for suffix in ["pl", "mi", "mul", "div"] {
        if let Some(t) = name.strip_suffix(suffix) {
            if TYPES.contains(&t) || t == "float48" || t == "float84" {
                return Some(Builtin::Op(match suffix {
                    "pl" => "+",
                    "mi" => "-",
                    "mul" => "*",
                    _ => "/",
                }));
            }
        }
    }
    if name == "numeric_add" {
        return Some(Builtin::Op("+"));
    }
    if name == "numeric_sub" {
        return Some(Builtin::Op("-"));
    }
    if name == "numeric_mul" {
        return Some(Builtin::Op("*"));
    }
    if name.ends_with("larger") {
        return Some(Builtin::Pick(std::cmp::Ordering::Greater));
    }
    if name.ends_with("smaller") {
        return Some(Builtin::Pick(std::cmp::Ordering::Less));
    }
    match name {
        "textcat" => Some(Builtin::Concat),
        "booland_statefunc" => Some(Builtin::And),
        "boolor_statefunc" => Some(Builtin::Or),
        n if crate::scalar::is_scalar(n) => Some(Builtin::Scalar),
        // A C function's own name (`int4abs`): the SQL built-in over it.
        n if internal_alias(n).is_some() => Some(Builtin::Scalar),
        _ => None,
    }
}

/// The SQL-callable built-in behind a C function's name (`int4abs` is
/// `abs`), when there is one.
fn internal_alias(name: &str) -> Option<String> {
    let sql = crate::user_ops::internal_call_sql(name, 1)
        .or_else(|| crate::user_ops::internal_call_sql(name, 2))?;
    let body = sql.strip_prefix("SELECT ")?;
    let (callee, _) = body.split_once('(')?;
    crate::scalar::is_scalar(callee).then(|| callee.to_string())
}

fn builtin_result_type(name: &str) -> Option<String> {
    crate::scalar::has_static_result_type(name)
        .then(|| crate::scalar::static_result_type(name).to_string())
}

#[derive(Clone, Copy)]
enum Builtin {
    Op(&'static str),
    Pick(std::cmp::Ordering),
    Concat,
    And,
    Or,
    Scalar,
}

/// Whether a built-in is STRICT (`proisstrict`). Most are; these few take
/// NULLs and do something with them -- `array_append(NULL, 1)` is `{1}` and
/// `array_append('{1}', NULL)` is `{1,NULL}`.
fn builtin_strict(name: &str) -> bool {
    !matches!(
        name,
        "array_append"
            | "array_prepend"
            | "array_cat"
            | "concat"
            | "concat_ws"
            | "format"
            | "num_nulls"
            | "num_nonnulls"
            | "json_build_array"
            | "jsonb_build_array"
            | "json_build_object"
            | "jsonb_build_object"
    )
}

fn call_builtin(name: &str, b: Builtin, args: &[Bson]) -> Result<Bson> {
    if builtin_strict(name) && args.contains(&Bson::Null) {
        return Ok(Bson::Null);
    }
    match b {
        Builtin::Op(op) => crate::eval_binary(op, args[0].clone(), args[1].clone()),
        Builtin::Pick(want) => Ok(if crate::compare_values(&args[1], &args[0]) == Some(want) {
            args[1].clone()
        } else {
            args[0].clone()
        }),
        Builtin::Concat => Ok(Bson::String(format!(
            "{}{}",
            crate::value_text(&args[0]),
            crate::value_text(&args[1])
        ))),
        Builtin::And | Builtin::Or => {
            let (a, c) = (args[0].as_bool(), args[1].as_bool());
            Ok(match (a, c, b) {
                (Some(a), Some(c), Builtin::And) => Bson::Boolean(a && c),
                (Some(a), Some(c), _) => Bson::Boolean(a || c),
                _ => Bson::Null,
            })
        }
        Builtin::Scalar => crate::scalar::call(name, args)
            .or_else(|| internal_alias(name).and_then(|alias| crate::scalar::call(&alias, args)))
            .unwrap_or_else(|| Err(Error::Unsupported(format!("the function {name}")))),
    }
}

/// A state or final function, resolved: a user function or a built-in.
enum Callee {
    User(UserFn),
    Builtin(String, Builtin),
}

impl Callee {
    fn strict(&self) -> bool {
        match self {
            Callee::User(u) => u.strict,
            Callee::Builtin(name, _) => builtin_strict(name),
        }
    }

    fn call(&self, args: &[Bson]) -> Result<Bson> {
        match self {
            Callee::User(u) => match call_user_function(u, args)? {
                FnResult::Value(v) => Ok(v),
                FnResult::Rows(..) => Err(Error::Sqlstate(
                    "0A000",
                    "a set-returning function as an aggregate's state function".into(),
                )),
            },
            Callee::Builtin(name, b) => call_builtin(name, *b, args),
        }
    }
}

fn callee(name: &str, arg_types: &[String]) -> Result<Callee> {
    if let Some(u) = user_fn(name, arg_types) {
        return Ok(Callee::User(u));
    }
    if let Some(b) = builtin(name) {
        return Ok(Callee::Builtin(name.to_string(), b));
    }
    // `DefineAggregate`'s lookup error carries no HINT.
    Err(Error::Sqlstate(
        "42883",
        format!(
            "function {name}({}) does not exist",
            arg_types
                .iter()
                .map(|t| crate::display_type(t))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

/// The argument types of a built-in operator function, read off its name as
/// `pg_proc` spells them: `int4pl(int4, int4)`, `int48mi(int4, int8)`,
/// `numeric_add(numeric, numeric)`, `textcat(text, text)`. `None` for a
/// function this does not know the signature of, which is not checked.
fn builtin_signature(name: &str) -> Option<Vec<&'static str>> {
    if crate::correlated::user_function_named(name) {
        return None;
    }
    let ops = [
        "pl", "mi", "mul", "div", "mod", "larger", "smaller", "eq", "ne", "lt", "le", "gt", "ge",
    ];
    if name == "textcat" || name == "text_larger" || name == "text_smaller" {
        return Some(vec!["text", "text"]);
    }
    if let Some(op) = name.strip_prefix("numeric_") {
        return matches!(
            op,
            "add" | "sub" | "mul" | "div" | "mod" | "larger" | "smaller"
        )
        .then(|| vec!["numeric", "numeric"]);
    }
    let width = |c: char| match c {
        '2' => Some("int2"),
        '4' => Some("int4"),
        '8' => Some("int8"),
        _ => None,
    };
    for float in ["float4", "float8"] {
        if let Some(op) = name.strip_prefix(float) {
            if ops.contains(&op) {
                return Some(vec![float, float]);
            }
        }
    }
    let rest = name.strip_prefix("int")?;
    let mut chars = rest.chars();
    let a = width(chars.next()?)?;
    let tail: String = chars.collect();
    if ops.contains(&tail.as_str()) {
        return Some(vec![a, a]);
    }
    let mut tail_chars = tail.chars();
    let b = width(tail_chars.next()?)?;
    let op: String = tail_chars.collect();
    ops.contains(&op.as_str()).then(|| vec![a, b])
}

/// 42883 when a built-in state or final function does not take these
/// argument types (`int4pl(integer, text)`), as `DefineAggregate` finds.
fn check_builtin_signature(name: &str, arg_types: &[String]) -> Result<()> {
    let missing = || {
        Error::Sqlstate(
            "42883",
            format!(
                "function {name}({}) does not exist",
                arg_types
                    .iter()
                    .map(|t| crate::display_type(t))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    };
    // PostgreSQL 15's own signatures first: every built-in is checked.
    if !crate::correlated::user_function_named(name) {
        match crate::funcsig::resolves(name, arg_types) {
            Some(true) => return Ok(()),
            Some(false) => return Err(missing()),
            None => {}
        }
    }
    let Some(want) = builtin_signature(name) else {
        return Ok(());
    };
    let same = |a: &str, b: &str| match (
        crate::pgtypes::oid_of_name(a),
        crate::pgtypes::oid_of_name(b),
    ) {
        (Some(x), Some(y)) => x == y,
        _ => a.eq_ignore_ascii_case(b),
    };
    if want.len() == arg_types.len() && want.iter().zip(arg_types).all(|(w, a)| same(w, a)) {
        return Ok(());
    }
    Err(missing())
}

/// Check a definition the way `DefineAggregate` does, before it is stored:
/// the required options, then that the state (and final) function exists
/// for these types.
pub fn validate(agg: &UserAggregate) -> Result<()> {
    if agg.sfunc.is_empty() {
        return Err(Error::Sqlstate(
            "42P13",
            "aggregate sfunc must be specified".into(),
        ));
    }
    if agg.stype.is_empty() {
        return Err(Error::Sqlstate(
            "42P13",
            "aggregate stype must be specified".into(),
        ));
    }
    let mut sig = vec![agg.stype.clone()];
    sig.extend(agg.arg_types.iter().cloned());
    callee(&agg.sfunc, &sig)?;
    check_builtin_signature(&agg.sfunc, &sig)?;
    if let Some(f) = &agg.finalfunc {
        check_builtin_signature(f, std::slice::from_ref(&agg.stype))?;
        match callee(f, std::slice::from_ref(&agg.stype))? {
            Callee::Builtin(_, Builtin::Scalar)
                if builtin_result_type(f).is_none()
                    && crate::funcsig::result_type(f, std::slice::from_ref(&agg.stype))
                        .is_none() =>
            {
                return Err(Error::Unsupported(format!(
                    "the built-in final function {f}"
                )))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Fold the aggregate over one group's values (NULLs included, in the
/// order the aggregate's ORDER BY gave them).
pub fn compute(agg: &UserAggregate, values: &[Bson], source: Option<&str>) -> Result<Bson> {
    let stype = state_type(agg, source);
    let value_type = source.unwrap_or("text").to_string();
    let sfunc = callee(&agg.sfunc, &[stype.clone(), value_type])?;
    let strict = sfunc.strict();
    let mut state = match &agg.initcond {
        Some(text) => crate::cast_value(Bson::String(text.clone()), &stype)?,
        None => Bson::Null,
    };
    // `noTransValue`: a strict state function with no initial condition has
    // no state until the first non-NULL input, which BECOMES the state.
    let mut no_trans_value = strict && agg.initcond.is_none();
    for v in values {
        if strict {
            if *v == Bson::Null {
                continue;
            }
            if no_trans_value {
                state = v.clone();
                no_trans_value = false;
                continue;
            }
            if state == Bson::Null {
                continue;
            }
        }
        state = sfunc.call(&[state, v.clone()])?;
    }
    match &agg.finalfunc {
        None => Ok(state),
        Some(f) => {
            let fin = callee(f, &[stype])?;
            if fin.strict() && state == Bson::Null {
                return Ok(Bson::Null);
            }
            fin.call(&[state])
        }
    }
}
