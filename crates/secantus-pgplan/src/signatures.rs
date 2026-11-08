//! PostgreSQL's function overload resolution (`func_select_candidate`) over
//! the built-in scalar functions this server implements, so a call whose
//! ARGUMENT TYPES match no overload is refused at plan time the way the
//! real server refuses it -- `md5(1)` is `42883 function md5(integer) does
//! not exist`, `trunc('a')` is `42725 function trunc(unknown) is not
//! unique` -- instead of being evaluated over whatever the value happens to
//! be. The signature table is `pg_proc` as measured on 16.15
//! (`pg_get_function_arguments` / `pg_get_function_result`), spelled in
//! internal type names.
//!
//! The rule, measured against 16.15: candidates of the call's arity (a
//! `VARIADIC "any"` absorbs any number of trailing arguments, but a
//! non-variadic candidate of the exact arity outranks it); keep those every
//! TYPED argument either equals or coerces IMPLICITLY to, an untyped input
//! (NULL, a string literal, an undeclared parameter) matching anything. None
//! left is 42883. Of several, the most exact matches win, then the most
//! preferred-type matches within an argument's category; still several and
//! every argument typed is 42725. An untyped argument then takes the
//! category its candidates agree on -- STRING wins a disagreement, any
//! other disagreement leaves it unresolved -- preferring the category's
//! preferred type when a candidate offers it. One candidate left is the
//! function; a typed argument's single common type as a last resort; else
//! 42725. The chosen candidate then reads every untyped literal as its
//! parameter type at parse time, so `abs('a')` is the 22P02 of
//! `double precision` at the literal.

use crate::common_type::{category, implicitly_coercible, is_preferred, is_untyped};
use crate::{call_argument_type, position_suffix, range, split_length_suffix, Error, Result};
use pg_query::protobuf::{node::Node as N, FuncCall, Node};

/// `(name, parameter types, result type, variadic)` -- a `true` variadic
/// means the LAST parameter absorbs any number of trailing arguments.
type Signature = (&'static str, &'static [&'static str], &'static str, bool);

const SIGNATURES: &[Signature] = &[
    ("abs", &["float4"], "float4", false),
    ("abs", &["float8"], "float8", false),
    ("abs", &["int2"], "int2", false),
    ("abs", &["int4"], "int4", false),
    ("abs", &["int8"], "int8", false),
    ("abs", &["numeric"], "numeric", false),
    ("area", &["box"], "float8", false),
    ("area", &["circle"], "float8", false),
    ("area", &["path"], "float8", false),
    ("array_dims", &["anyarray"], "text", false),
    ("array_length", &["anyarray", "int4"], "int4", false),
    ("array_lower", &["anyarray", "int4"], "int4", false),
    ("array_ndims", &["anyarray"], "int4", false),
    ("array_to_string", &["anyarray", "text"], "text", false),
    (
        "array_to_string",
        &["anyarray", "text", "text"],
        "text",
        false,
    ),
    ("array_upper", &["anyarray", "int4"], "int4", false),
    ("ascii", &["text"], "int4", false),
    ("bit_length", &["bit"], "int4", false),
    ("bit_length", &["bytea"], "int4", false),
    ("bit_length", &["text"], "int4", false),
    ("bound_box", &["box", "box"], "box", false),
    ("btrim", &["bytea", "bytea"], "bytea", false),
    ("btrim", &["text"], "text", false),
    ("btrim", &["text", "text"], "text", false),
    ("cardinality", &["anyarray"], "int4", false),
    ("ceil", &["float8"], "float8", false),
    ("ceil", &["numeric"], "numeric", false),
    ("ceiling", &["float8"], "float8", false),
    ("ceiling", &["numeric"], "numeric", false),
    ("char_length", &["bpchar"], "int4", false),
    ("char_length", &["text"], "int4", false),
    ("character_length", &["bpchar"], "int4", false),
    ("character_length", &["text"], "int4", false),
    ("chr", &["int4"], "text", false),
    ("clock_timestamp", &[], "timestamptz", false),
    ("concat", &["any"], "text", true),
    ("concat_ws", &["text", "any"], "text", true),
    ("decode", &["text", "text"], "bytea", false),
    ("div", &["numeric", "numeric"], "numeric", false),
    ("encode", &["bytea", "text"], "text", false),
    ("exp", &["float8"], "float8", false),
    ("exp", &["numeric"], "numeric", false),
    ("floor", &["float8"], "float8", false),
    ("floor", &["numeric"], "numeric", false),
    ("format", &["text"], "text", false),
    ("format", &["text", "any"], "text", true),
    ("format_type", &["oid", "int4"], "text", false),
    ("gen_random_uuid", &[], "uuid", false),
    ("get_byte", &["bytea", "int4"], "int4", false),
    ("height", &["box"], "float8", false),
    ("initcap", &["text"], "text", false),
    ("json_build_array", &[], "json", false),
    ("json_build_array", &["any"], "json", true),
    ("json_build_object", &[], "json", false),
    ("json_build_object", &["any"], "json", true),
    ("jsonb_build_array", &[], "jsonb", false),
    ("jsonb_build_array", &["any"], "jsonb", true),
    ("jsonb_build_object", &[], "jsonb", false),
    ("jsonb_build_object", &["any"], "jsonb", true),
    ("left", &["text", "int4"], "text", false),
    ("length", &["bit"], "int4", false),
    ("length", &["bpchar"], "int4", false),
    ("length", &["bytea"], "int4", false),
    ("length", &["bytea", "name"], "int4", false),
    ("length", &["lseg"], "float8", false),
    ("length", &["path"], "float8", false),
    ("length", &["text"], "int4", false),
    ("length", &["tsvector"], "int4", false),
    ("ln", &["float8"], "float8", false),
    ("ln", &["numeric"], "numeric", false),
    ("log", &["float8"], "float8", false),
    ("log", &["numeric"], "numeric", false),
    ("log", &["numeric", "numeric"], "numeric", false),
    ("log10", &["float8"], "float8", false),
    ("log10", &["numeric"], "numeric", false),
    ("lower", &["anymultirange"], "anyelement", false),
    ("lower", &["anyrange"], "anyelement", false),
    ("lower", &["text"], "text", false),
    ("lpad", &["text", "int4"], "text", false),
    ("lpad", &["text", "int4", "text"], "text", false),
    ("ltrim", &["bytea", "bytea"], "bytea", false),
    ("ltrim", &["text"], "text", false),
    ("ltrim", &["text", "text"], "text", false),
    ("md5", &["bytea"], "text", false),
    ("md5", &["text"], "text", false),
    ("mod", &["int2", "int2"], "int2", false),
    ("mod", &["int4", "int4"], "int4", false),
    ("mod", &["int8", "int8"], "int8", false),
    ("mod", &["numeric", "numeric"], "numeric", false),
    ("now", &[], "timestamptz", false),
    ("octet_length", &["bit"], "int4", false),
    ("octet_length", &["bpchar"], "int4", false),
    ("octet_length", &["bytea"], "int4", false),
    ("octet_length", &["text"], "int4", false),
    ("pg_function_is_visible", &["oid"], "bool", false),
    ("pg_get_constraintdef", &["oid"], "text", false),
    ("pg_get_constraintdef", &["oid", "bool"], "text", false),
    ("pg_get_expr", &["text", "oid"], "text", false),
    ("pg_get_expr", &["text", "oid", "bool"], "text", false),
    ("pg_get_indexdef", &["oid"], "text", false),
    ("pg_get_indexdef", &["oid", "int4", "bool"], "text", false),
    ("pg_get_userbyid", &["oid"], "name", false),
    ("pg_table_is_visible", &["oid"], "bool", false),
    ("pg_type_is_visible", &["oid"], "bool", false),
    ("pi", &[], "float8", false),
    ("generate_series", &["int8", "int8"], "int8", false),
    ("generate_series", &["int8", "int8", "int8"], "int8", false),
    ("generate_series", &["int4", "int4"], "int4", false),
    ("generate_series", &["int4", "int4", "int4"], "int4", false),
    ("generate_series", &["numeric", "numeric"], "numeric", false),
    (
        "generate_series",
        &["numeric", "numeric", "numeric"],
        "numeric",
        false,
    ),
    (
        "generate_series",
        &["timestamptz", "timestamptz", "interval"],
        "timestamptz",
        false,
    ),
    (
        "generate_series",
        &["timestamptz", "timestamptz", "interval", "text"],
        "timestamptz",
        false,
    ),
    (
        "generate_series",
        &["timestamp", "timestamp", "interval"],
        "timestamp",
        false,
    ),
    ("position", &["bit", "bit"], "int4", false),
    ("position", &["bytea", "bytea"], "int4", false),
    ("position", &["text", "text"], "int4", false),
    ("pow", &["float8", "float8"], "float8", false),
    ("pow", &["numeric", "numeric"], "numeric", false),
    ("power", &["float8", "float8"], "float8", false),
    ("power", &["numeric", "numeric"], "numeric", false),
    ("quote_ident", &["text"], "text", false),
    ("quote_literal", &["anyelement"], "text", false),
    ("quote_literal", &["text"], "text", false),
    ("quote_nullable", &["anyelement"], "text", false),
    ("quote_nullable", &["text"], "text", false),
    ("random", &[], "float8", false),
    ("repeat", &["text", "int4"], "text", false),
    ("replace", &["text", "text", "text"], "text", false),
    ("reverse", &["text"], "text", false),
    ("right", &["text", "int4"], "text", false),
    ("round", &["float8"], "float8", false),
    ("round", &["numeric"], "numeric", false),
    ("round", &["numeric", "int4"], "numeric", false),
    ("rpad", &["text", "int4"], "text", false),
    ("rpad", &["text", "int4", "text"], "text", false),
    ("rtrim", &["bytea", "bytea"], "bytea", false),
    ("rtrim", &["text"], "text", false),
    ("rtrim", &["text", "text"], "text", false),
    ("set_byte", &["bytea", "int4", "int4"], "bytea", false),
    ("sign", &["float8"], "float8", false),
    ("sign", &["numeric"], "numeric", false),
    ("split_part", &["text", "text", "int4"], "text", false),
    ("sqrt", &["float8"], "float8", false),
    ("sqrt", &["numeric"], "numeric", false),
    ("starts_with", &["text", "text"], "bool", false),
    ("statement_timestamp", &[], "timestamptz", false),
    ("strpos", &["text", "text"], "int4", false),
    ("substr", &["bytea", "int4"], "bytea", false),
    ("substr", &["bytea", "int4", "int4"], "bytea", false),
    ("substr", &["text", "int4"], "text", false),
    ("substr", &["text", "int4", "int4"], "text", false),
    ("substring", &["bit", "int4"], "bit", false),
    ("substring", &["bit", "int4", "int4"], "bit", false),
    ("substring", &["bytea", "int4"], "bytea", false),
    ("substring", &["bytea", "int4", "int4"], "bytea", false),
    ("substring", &["text", "int4"], "text", false),
    ("substring", &["text", "int4", "int4"], "text", false),
    ("substring", &["text", "text"], "text", false),
    ("substring", &["text", "text", "text"], "text", false),
    ("to_char", &["float4", "text"], "text", false),
    ("to_char", &["float8", "text"], "text", false),
    ("to_char", &["int4", "text"], "text", false),
    ("to_char", &["int8", "text"], "text", false),
    ("to_char", &["interval", "text"], "text", false),
    ("to_char", &["numeric", "text"], "text", false),
    ("to_char", &["timestamp", "text"], "text", false),
    ("to_char", &["timestamptz", "text"], "text", false),
    ("to_json", &["anyelement"], "json", false),
    ("to_jsonb", &["anyelement"], "jsonb", false),
    ("transaction_timestamp", &[], "timestamptz", false),
    ("translate", &["text", "text", "text"], "text", false),
    ("trunc", &["float8"], "float8", false),
    ("trunc", &["macaddr"], "macaddr", false),
    ("trunc", &["macaddr8"], "macaddr8", false),
    ("trunc", &["numeric"], "numeric", false),
    ("trunc", &["numeric", "int4"], "numeric", false),
    ("upper", &["anymultirange"], "anyelement", false),
    ("upper", &["anyrange"], "anyelement", false),
    ("upper", &["text"], "text", false),
    ("width", &["box"], "float8", false),
];

/// The overload PostgreSQL selects for `f`: its parameter types (one per
/// argument, the variadic parameter repeated) and its result type, or
/// `None` for a polymorphic result. `Ok(None)` when the name is not in the
/// table, the call is not a plain scalar call (an aggregate form, a window
/// call), or an argument's type cannot be named without planning it.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub(crate) params: Vec<&'static str>,
    pub(crate) result: Option<&'static str>,
}

/// An argument as the resolver sees it.
#[derive(Clone, PartialEq, Eq)]
enum Arg {
    /// A NULL, an untyped literal or an undeclared parameter.
    Unknown,
    /// A type this server has a category record for, in internal spelling.
    Typed(String),
}

/// The internal spelling of a display type name (`display_type`'s inverse),
/// any length modifier dropped.
fn internal_type(display: &str) -> String {
    if let Some(base) = display.strip_suffix("[]") {
        return format!("{}[]", internal_type(base));
    }
    let (base, _) = split_length_suffix(display);
    match base {
        "integer" | "int" => "int4",
        "smallint" => "int2",
        "bigint" => "int8",
        "real" => "float4",
        "double precision" | "double" => "float8",
        "decimal" => "numeric",
        "boolean" => "bool",
        "character varying" => "varchar",
        "character" => "bpchar",
        "\"char\"" => "char",
        "time without time zone" => "time",
        "timestamp without time zone" => "timestamp",
        "timestamp with time zone" => "timestamptz",
        "time with time zone" => "timetz",
        "bit varying" => "varbit",
        "pg_node_tree" => "text",
        other => other,
    }
    .to_string()
}

/// Whether `t` is a type the table can speak for. A user-defined type -- a
/// domain, an enum, a composite -- has no category record here and PostgreSQL
/// resolves it through casts this server does not carry, so a call over one
/// is left to the value path rather than refused on a guess.
fn known_type(t: &str) -> bool {
    let base = t.strip_suffix("[]").unwrap_or(t);
    category(base) != 'U'
        || matches!(
            base,
            "bytea" | "json" | "jsonb" | "uuid" | "macaddr" | "macaddr8" | "tsvector" | "xml"
        )
}

fn argument(node: &Node) -> Option<Arg> {
    if is_untyped(node) {
        return Some(Arg::Unknown);
    }
    let display = call_argument_type(node)?;
    if display == "unknown" {
        return Some(Arg::Unknown);
    }
    let t = internal_type(&display);
    known_type(&t).then_some(Arg::Typed(t))
}

fn is_polymorphic(param: &str) -> bool {
    matches!(
        param,
        "any" | "anyelement" | "anyarray" | "anyrange" | "anymultirange"
    )
}

/// Whether a typed argument of `t` can be passed for `param`.
fn accepts(param: &str, t: &str) -> bool {
    match param {
        "any" | "anyelement" => true,
        "anyarray" => t.ends_with("[]"),
        "anyrange" => range::is_range_type(t),
        "anymultirange" => range::is_multirange_type(t),
        _ => t == param || implicitly_coercible(t, param),
    }
}

/// The call's name as PostgreSQL prints it in a message: the schema kept
/// when one was written (`trim(both from 1)` parses as
/// `pg_catalog.btrim(1)`).
fn printed_name(f: &FuncCall) -> Option<(String, &str)> {
    let parts: Vec<&str> = f
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref()? {
            N::String(st) => Some(st.sval.as_str()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [name] => Some((name.to_string(), name)),
        ["pg_catalog", name] => Some((format!("pg_catalog.{name}"), name)),
        _ => None,
    }
}

/// Whether `f` is a plain scalar call the table can judge.
fn plain_call(f: &FuncCall) -> bool {
    !f.agg_star
        && !f.agg_distinct
        && !f.func_variadic
        && f.agg_order.is_empty()
        && f.agg_filter.is_none()
        && f.over.is_none()
}

pub(crate) fn resolve(f: &FuncCall) -> Result<Option<Resolved>> {
    if !plain_call(f) {
        return Ok(None);
    }
    let Some((printed, bare)) = printed_name(f) else {
        return Ok(None);
    };
    let overloads: Vec<&Signature> = SIGNATURES.iter().filter(|s| s.0 == bare).collect();
    if overloads.is_empty() {
        return Ok(None);
    }
    let Some(args) = f.args.iter().map(argument).collect::<Option<Vec<_>>>() else {
        return Ok(None);
    };
    let nargs = args.len();
    let arity: Vec<&Signature> = overloads
        .iter()
        .copied()
        .filter(|(_, params, _, variadic)| {
            if *variadic {
                nargs + 1 >= params.len()
            } else {
                params.len() == nargs
            }
        })
        .collect();
    let arity: Vec<&Signature> = if arity.iter().any(|(_, _, _, v)| !v) {
        arity.into_iter().filter(|(_, _, _, v)| !v).collect()
    } else {
        arity
    };
    let expanded: Vec<(Vec<&'static str>, &'static str)> = arity
        .iter()
        .map(|(_, params, result, variadic)| {
            let mut ps: Vec<&'static str> = params.to_vec();
            if *variadic {
                let last = ps.pop().unwrap_or("any");
                while ps.len() < nargs {
                    ps.push(last);
                }
            }
            (ps, *result)
        })
        .collect();
    let mut candidates: Vec<&(Vec<&'static str>, &'static str)> = expanded
        .iter()
        .filter(|(ps, _)| {
            ps.iter().zip(&args).all(|(p, a)| match a {
                Arg::Unknown => true,
                Arg::Typed(t) => accepts(p, t),
            })
        })
        .collect();
    let types = || {
        args.iter()
            .map(|a| match a {
                Arg::Unknown => "unknown".to_string(),
                Arg::Typed(t) => crate::display_type(t),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let pos = position_suffix(f.location);
    if candidates.is_empty() {
        return Err(Error::UnknownFunction {
            name: bare.to_string(),
            message: format!("function {printed}({}) does not exist{pos}", types()),
        });
    }
    let ambiguous = || {
        Error::AmbiguousOperator(format!(
            "function {printed}({}) is not unique{pos}",
            types()
        ))
    };
    let chosen = if candidates.len() == 1 {
        candidates[0]
    } else {
        let keep_max = |cands: &mut Vec<&(Vec<&'static str>, &'static str)>,
                        score: &dyn Fn(&[&'static str]) -> usize| {
            let best = cands.iter().map(|(ps, _)| score(ps)).max().unwrap_or(0);
            cands.retain(|(ps, _)| score(ps) == best);
        };
        keep_max(&mut candidates, &|ps| {
            ps.iter()
                .zip(&args)
                .filter(|(p, a)| matches!(a, Arg::Typed(t) if t == *p))
                .count()
        });
        if candidates.len() > 1 {
            keep_max(&mut candidates, &|ps| {
                ps.iter()
                    .zip(&args)
                    .filter(|(p, a)| {
                        matches!(a, Arg::Typed(t) if t != *p && is_preferred(p) && category(t) == category(p))
                    })
                    .count()
            });
        }
        if candidates.len() > 1 {
            if !args.contains(&Arg::Unknown) {
                return Err(ambiguous());
            }
            // Each unknown argument takes the category its candidates agree on.
            let mut resolved = true;
            let mut slots: Vec<Option<(char, bool)>> = vec![None; nargs];
            for (i, a) in args.iter().enumerate() {
                if *a != Arg::Unknown {
                    continue;
                }
                let mut slot: Option<(char, bool)> = None;
                let mut conflict = false;
                for (ps, _) in &candidates {
                    let cat = category(ps[i]);
                    let pref = is_preferred(ps[i]);
                    slot = Some(match slot {
                        None => (cat, pref),
                        Some((c, p)) if c == cat => (c, p || pref),
                        Some(_) if cat == 'S' => (cat, pref),
                        Some(s) => {
                            conflict = true;
                            s
                        }
                    });
                }
                if conflict && slot.is_some_and(|(c, _)| c != 'S') {
                    resolved = false;
                    break;
                }
                slots[i] = slot;
            }
            if resolved {
                candidates.retain(|(ps, _)| {
                    slots.iter().enumerate().all(|(i, slot)| match slot {
                        None => true,
                        Some((c, pref)) => category(ps[i]) == *c && (!pref || is_preferred(ps[i])),
                    })
                });
            }
            if candidates.len() != 1 {
                // Every typed argument of one type: read the unknowns as it.
                let mut known: Option<&str> = None;
                let mut single = true;
                for a in &args {
                    if let Arg::Typed(t) = a {
                        match known {
                            None => known = Some(t),
                            Some(k) if k == t => {}
                            Some(_) => single = false,
                        }
                    }
                }
                let exact: Vec<&(Vec<&'static str>, &'static str)> = match (single, known) {
                    (true, Some(k)) => expanded
                        .iter()
                        .filter(|(ps, _)| ps.iter().all(|p| *p == k))
                        .collect(),
                    _ => Vec::new(),
                };
                if exact.len() == 1 {
                    candidates = exact;
                } else {
                    return Err(ambiguous());
                }
            }
        }
        candidates[0]
    };
    let (params, result) = chosen;
    if params
        .iter()
        .zip(&args)
        .any(|(p, a)| *a == Arg::Unknown && is_polymorphic(p) && *p != "any")
    {
        return Err(Error::DatatypeMismatch(
            "could not determine polymorphic type because input has type unknown".to_string(),
        ));
    }
    Ok(Some(Resolved {
        params: params.clone(),
        result: (!is_polymorphic(result)).then_some(result),
    }))
}

/// Refuse `f` the way PostgreSQL would at plan time: no matching overload,
/// an ambiguous one, or an untyped literal its parameter type cannot read.
pub(crate) fn check_call(f: &FuncCall) -> Result<()> {
    let Some(resolved) = resolve(f)? else {
        return Ok(());
    };
    for (node, param) in f.args.iter().zip(&resolved.params) {
        if literal_is_parsed(param) {
            crate::common_type::validate_literal(node, param)?;
        }
    }
    Ok(())
}

/// Whether an untyped literal passed for `param` is READ at parse time,
/// where a malformed one fails: the numeric, boolean, date/time, interval
/// and bit families. A string family parameter takes any text, a
/// polymorphic one leaves the literal as it is.
pub(crate) fn literal_is_parsed(param: &str) -> bool {
    matches!(category(param), 'N' | 'B' | 'D' | 'T' | 'V')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(sql: &str) -> FuncCall {
        let parsed = crate::parse_one(sql).expect("parses");
        let mut probe = Node { node: Some(parsed) };
        let mut found = None;
        crate::walk::walk(&mut probe, &mut |n| {
            if let Some(N::FuncCall(f)) = n.node.as_ref() {
                if found.is_none() {
                    found = Some((**f).clone());
                }
            }
            false
        });
        found.expect("a call")
    }

    #[test]
    fn exact_and_coerced_overloads() {
        let r = resolve(&call("select abs(1)")).unwrap().unwrap();
        assert_eq!(r.params, vec!["int4"]);
        assert_eq!(r.result, Some("int4"));
        let r = resolve(&call("select power(2, 3)")).unwrap().unwrap();
        assert_eq!(r.params, vec!["float8", "float8"]);
        let r = resolve(&call("select mod(1::int2, 2::int8)"))
            .unwrap()
            .unwrap();
        assert_eq!(r.params, vec!["int8", "int8"]);
        let r = resolve(&call("select length(null)")).unwrap().unwrap();
        assert_eq!(r.params, vec!["text"]);
        let r = resolve(&call("select abs(null)")).unwrap().unwrap();
        assert_eq!(r.result, Some("float8"));
    }

    #[test]
    fn refusals() {
        let err = resolve(&call("select md5(1)")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "function md5(integer) does not exist\nPosition: 7"
        );
        let err = resolve(&call("select trunc('a')")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "function trunc(unknown) is not unique\nPosition: 7"
        );
        let err = resolve(&call("select trim(both from 1)")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "function pg_catalog.btrim(integer) does not exist\nPosition: 7"
        );
        let err = resolve(&call("select to_json('a')")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "could not determine polymorphic type because input has type unknown"
        );
        let err = check_call(&call("select abs('a')")).unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid input syntax for type double precision: \"a\"\nPosition: 11"
        );
    }
}
