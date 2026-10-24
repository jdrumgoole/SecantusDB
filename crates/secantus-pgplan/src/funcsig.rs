//! Built-in function resolution by argument TYPE, as `func_select_candidate`
//! narrows it: a call to a `pg_catalog` function whose known argument types
//! no overload can take (exactly, through an implicit cast, or through a
//! polymorphic parameter) is PostgreSQL's 42883 `function f(types) does not
//! exist` -- where lowering it anyway answered a runtime error, or a value.
//!
//! The overloads (`pg_proc_sigs.tsv`: name, argument types, variadic, number
//! of defaults) and the implicit casts (`pg_implicit_casts.tsv`) were read
//! from PostgreSQL 15.19's catalogs.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

const SIGS: &str = include_str!("pg_proc_sigs.tsv");
const CASTS: &str = include_str!("pg_implicit_casts.tsv");
/// `(type, typcategory, typispreferred)` of every `pg_catalog` type, arrays
/// spelled `elem[]`, read from PostgreSQL 15.19.
const CATEGORIES: &str = include_str!("pg_type_category.tsv");
/// PostgreSQL 15.19's binary operators (`pg_operator`, `oprkind = 'b'`,
/// oids below 16384): name, left type, right type, result type, arrays
/// spelled `elem[]`.
const OPERATORS: &str = include_str!("pg_binary_operators.tsv");

struct Sig {
    args: Vec<String>,
    variadic: bool,
    defaults: usize,
    ret: String,
    retset: bool,
}

/// The catalog's name for a type as the planner spells it: `pg_proc` calls
/// the one-byte type `char` (the SQL `character` is `bpchar` there), which
/// is `"char"` everywhere else in this crate.
fn planner_type(t: &str) -> String {
    match t {
        "char" => crate::QUOTED_CHAR.to_string(),
        "char[]" => format!("{}[]", crate::QUOTED_CHAR),
        other => other.to_string(),
    }
}

fn sigs() -> &'static HashMap<String, Vec<Sig>> {
    static S: OnceLock<HashMap<String, Vec<Sig>>> = OnceLock::new();
    S.get_or_init(|| {
        let mut m: HashMap<String, Vec<Sig>> = HashMap::new();
        for l in SIGS.lines() {
            let mut it = l.split('\t');
            let (Some(name), Some(args), Some(v), Some(d), Some(ret), Some(set)) = (
                it.next(),
                it.next(),
                it.next(),
                it.next(),
                it.next(),
                it.next(),
            ) else {
                continue;
            };
            m.entry(name.to_string()).or_default().push(Sig {
                args: if args.is_empty() {
                    Vec::new()
                } else {
                    args.split(',').map(planner_type).collect()
                },
                variadic: v == "1",
                defaults: d.parse().unwrap_or(0),
                ret: planner_type(ret),
                retset: set == "1",
            });
        }
        m
    })
}

fn casts() -> &'static HashSet<(String, String)> {
    static C: OnceLock<HashSet<(String, String)>> = OnceLock::new();
    C.get_or_init(|| {
        CASTS
            .lines()
            .filter_map(|l| {
                let (a, b) = l.split_once('\t')?;
                Some((planner_type(a), planner_type(b)))
            })
            .collect()
    })
}

/// Can an argument of type `arg` go where `param` is declared?
fn accepts(param: &str, arg: &str) -> bool {
    if arg.is_empty() || arg == "unknown" || param == arg {
        return true;
    }
    if param == "pg_node_tree" && arg == "text" {
        return true;
    }
    match param {
        "any" | "anyelement" | "anycompatible" => return true,
        "anyarray" | "anycompatiblearray" => return arg.ends_with("[]"),
        "anynonarray" | "anycompatiblenonarray" => return !arg.ends_with("[]"),
        // An enum or a range is a user type here; anything not built in
        // is given the benefit of the doubt.
        "anyenum"
        | "anyrange"
        | "anymultirange"
        | "anycompatiblerange"
        | "anycompatiblemultirange" => {
            return crate::pgtypes::oid_of_name(arg).is_none() || arg.contains("range")
        }
        "record" => return crate::pgtypes::oid_of_name(arg).is_none(),
        _ => {}
    }
    // A type this server does not know built in (a domain, an enum, a
    // composite) is not judged.
    let known = |t: &str| crate::pgtypes::oid_of_name(t.trim_end_matches("[]")).is_some();
    if !known(arg) {
        return true;
    }
    if let (Some(pe), Some(ae)) = (param.strip_suffix("[]"), arg.strip_suffix("[]")) {
        return accepts(pe, ae);
    }
    // The string types coerce among themselves.
    let stringish = |t: &str| matches!(t, "text" | "varchar" | "bpchar" | "name");
    if stringish(param) && stringish(arg) {
        return true;
    }
    casts().contains(&(arg.to_string(), param.to_string()))
}

/// `Some(false)` when `name` is a built-in and no overload takes `args`
/// (their types as the planner knows them, `""` for unknown); `None` when
/// `name` is not a built-in this table has.
pub(crate) fn resolves(name: &str, args: &[String]) -> Option<bool> {
    // The catalog readers take this server's catalog columns, whose types
    // approximate PostgreSQL's (a `pg_node_tree` is text here): not judged.
    if crate::scalar::is_catalog_reader(name) || name.starts_with("pg_") {
        return None;
    }
    let overloads = sigs().get(name)?;
    Some(overloads.iter().any(|s| sig_accepts(s, args)))
}

/// The type a call returns: the overload `func_select_candidate` picks, or
/// else the one concrete (non-polymorphic, non-set) type every overload
/// that takes `args` returns.
pub(crate) fn result_type(name: &str, args: &[String]) -> Option<String> {
    if let Some(t) = selected_result_type(name, args) {
        return Some(t);
    }
    let overloads = sigs().get(name)?;
    let mut out: Option<&str> = None;
    for s in overloads.iter().filter(|s| sig_accepts(s, args)) {
        if s.retset
            || s.ret.starts_with("any")
            || matches!(
                s.ret.as_str(),
                "record" | "void" | "internal" | "trigger" | "unknown"
            )
        {
            return None;
        }
        match out {
            None => out = Some(&s.ret),
            Some(o) if o == s.ret => {}
            Some(_) => return None,
        }
    }
    out.map(str::to_string)
}

fn categories() -> &'static HashMap<String, (char, bool)> {
    static C: OnceLock<HashMap<String, (char, bool)>> = OnceLock::new();
    C.get_or_init(|| {
        CATEGORIES
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let name = it.next()?;
                let cat = it.next()?.chars().next()?;
                let preferred = it.next()? == "1";
                Some((planner_type(name), (cat, preferred)))
            })
            .collect()
    })
}

fn binary_operators() -> &'static HashMap<String, Vec<(String, String)>> {
    static O: OnceLock<HashMap<String, Vec<(String, String)>>> = OnceLock::new();
    O.get_or_init(|| {
        let mut m: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for l in OPERATORS.lines() {
            let mut it = l.split('\t');
            let (Some(name), Some(left), Some(right)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            m.entry(name.to_string())
                .or_default()
                .push((planner_type(left), planner_type(right)));
        }
        m
    })
}

/// `Some(false)` when no built-in binary operator `op` takes a left operand
/// of type `l` and a right one of type `r` -- exactly, through an implicit
/// cast or through a polymorphic parameter -- which is PostgreSQL's 42883
/// `operator does not exist`. `None` when either type is not a built-in
/// this table knows (a domain, an enum, an extension type, `unknown`) or
/// `op` is not a built-in operator: those are not judged.
pub(crate) fn operator_resolves(op: &str, l: &str, r: &str) -> Option<bool> {
    let cats = categories();
    if l == "unknown" || r == "unknown" || !cats.contains_key(l) || !cats.contains_key(r) {
        return None;
    }
    let cands = binary_operators().get(op)?;
    Some(cands.iter().any(|(a, b)| accepts(a, l) && accepts(b, r)))
}

/// Is `l op r` over two known built-in types AMBIGUOUS -- PostgreSQL's
/// 42725 `operator is not unique`? `oper_select_candidate` narrows the
/// operators that take the operands (exactly or by implicit cast) as
/// `func_select_candidate` narrows functions: most exact matches, then most
/// exact-or-preferred matches; more than one left is ambiguous (`macaddr =
/// macaddr8`, each castable to the other, measured on 15.19). Not judged
/// (false) for arrays, `unknown`, or a polymorphic candidate.
pub(crate) fn operator_ambiguous(op: &str, l: &str, r: &str) -> bool {
    let cats = categories();
    if l == "unknown"
        || r == "unknown"
        || l.ends_with("[]")
        || r.ends_with("[]")
        || !cats.contains_key(l)
        || !cats.contains_key(r)
    {
        return false;
    }
    let Some(all) = binary_operators().get(op) else {
        return false;
    };
    let mut cands: Vec<&(String, String)> = all
        .iter()
        .filter(|(a, b)| accepts(a, l) && accepts(b, r))
        .collect();
    let args = [l, r];
    let params = |c: &(String, String)| [c.0.clone(), c.1.clone()];
    if cands.len() < 2
        || cands.iter().any(|c| *c.0 == *l && *c.1 == *r)
        || cands.iter().flat_map(|c| params(c)).any(|p| {
            p.starts_with("any") || matches!(p.as_str(), "record" | "internal" | "cstring")
        })
    {
        return false;
    }
    let keep_best = |cands: &mut Vec<&(String, String)>, score: &dyn Fn(&str, &str) -> bool| {
        let count = |c: &&(String, String)| {
            params(c)
                .iter()
                .zip(args)
                .filter(|(p, a)| score(p, a))
                .count()
        };
        let best = cands.iter().map(count).max().unwrap_or(0);
        cands.retain(|c| count(c) == best);
    };
    keep_best(&mut cands, &|p, a| p == a);
    keep_best(&mut cands, &|p, a| {
        p == a
            || matches!(
                (cats.get(p), cats.get(a)),
                (Some((pc, true)), Some((ac, _))) if pc == ac
            )
    });
    cands.len() > 1
}

/// The LEFT input type of the binary operator `op` that PostgreSQL resolves
/// for `l op r` (`oper_select_candidate`): `l` itself when an operator takes
/// `(l, r)` exactly, otherwise the one candidate left after keeping the most
/// exact matches, then the most exact-or-preferred ones. `None` when the
/// types are not both known built-ins or no single candidate remains. NULLIF
/// takes this type (RP-076): `int4 = numeric` resolves to `numeric =
/// numeric`, so `nullif(53, 80::numeric)` is numeric, while `int2 = int8`
/// is the cross-type `int28eq` and keeps `int2`.
pub(crate) fn operator_left_input(op: &str, l: &str, r: &str) -> Option<String> {
    let cats = categories();
    if l == "unknown"
        || r == "unknown"
        || l.ends_with("[]")
        || r.ends_with("[]")
        || !cats.contains_key(l)
        || !cats.contains_key(r)
    {
        return None;
    }
    let all = binary_operators().get(op)?;
    if all.iter().any(|(a, b)| a == l && b == r) {
        return Some(l.to_string());
    }
    let mut cands: Vec<&(String, String)> = all
        .iter()
        .filter(|(a, b)| accepts(a, l) && accepts(b, r))
        .collect();
    if cands.iter().any(|c| {
        [&c.0, &c.1].iter().any(|p| {
            p.starts_with("any") || matches!(p.as_str(), "record" | "internal" | "cstring")
        })
    }) {
        return None;
    }
    let args = [l, r];
    let keep_best = |cands: &mut Vec<&(String, String)>, score: &dyn Fn(&str, &str) -> bool| {
        let count = |c: &&(String, String)| {
            [c.0.as_str(), c.1.as_str()]
                .iter()
                .zip(args)
                .filter(|(p, a)| score(p, a))
                .count()
        };
        let best = cands.iter().map(count).max().unwrap_or(0);
        cands.retain(|c| count(c) == best);
    };
    keep_best(&mut cands, &|p, a| p == a);
    keep_best(&mut cands, &|p, a| {
        p == a
            || matches!(
                (cats.get(p), cats.get(a)),
                (Some((pc, true)), Some((ac, _))) if pc == ac
            )
    });
    match cands.as_slice() {
        [one] => Some(one.0.clone()),
        _ => None,
    }
}

/// `pg_type.typcategory` and `typispreferred` of a built-in type, by oid.
pub fn category_preferred_of_oid(oid: i64) -> Option<(char, bool)> {
    let name = crate::pgtypes::type_name_of_oid(oid)?;
    categories().get(&planner_type(&name)).copied()
}

/// `pg_type.typcategory` of a built-in type, by oid.
pub(crate) fn category_of_oid(oid: i64) -> Option<char> {
    let name = crate::pgtypes::name_of_oid(oid)?;
    categories().get(&planner_type(name)).map(|c| c.0)
}

/// The declared type of argument `i` under overload `s` (a variadic's
/// element past its fixed arguments).
fn param_at(s: &Sig, i: usize) -> Option<&str> {
    let fixed = if s.variadic {
        s.args.len().saturating_sub(1)
    } else {
        s.args.len()
    };
    if i < fixed || !s.variadic {
        return s.args.get(i).map(String::as_str);
    }
    let v = s.args.last().map(String::as_str).unwrap_or("any");
    Some(if v == "any" {
        "any"
    } else {
        v.strip_suffix("[]").unwrap_or(v)
    })
}

/// PostgreSQL's `func_select_candidate` over the overloads that take `args`:
/// an exact match wins; otherwise keep those with the most EXACT matches,
/// then those with the most positions where the parameter is exact or the
/// PREFERRED type of the argument's category (`round(int4)` is the `float8`
/// one, not the `numeric` one). `Some(survivors)` -- one is the winner, more
/// is ambiguous (42725) -- and `None` where PostgreSQL's rules are not fully
/// modelled here: an argument that is not a known built-in type, or a
/// candidate with a polymorphic parameter.
fn select_candidates<'a>(name: &str, args: &[String]) -> Option<Vec<&'a Sig>> {
    if crate::scalar::is_catalog_reader(name) || name.starts_with("pg_") || args.is_empty() {
        return None;
    }
    let cats = categories();
    if args
        .iter()
        .any(|a| !cats.contains_key(a.as_str()) || a == "unknown")
    {
        return None;
    }
    let overloads = sigs().get(name)?;
    let mut cands: Vec<&Sig> = overloads.iter().filter(|s| sig_accepts(s, args)).collect();
    if cands.is_empty() {
        return None;
    }
    let params = |s: &Sig| -> Vec<String> {
        (0..args.len())
            .map(|i| param_at(s, i).unwrap_or("any").to_string())
            .collect()
    };
    if cands
        .iter()
        .flat_map(|s| params(s))
        .any(|p| p.starts_with("any") || matches!(p.as_str(), "record" | "internal" | "cstring"))
    {
        return None;
    }
    if let Some(exact) = cands
        .iter()
        .find(|s| params(s).iter().zip(args).all(|(p, a)| p == a))
    {
        return Some(vec![*exact]);
    }
    let keep_best = |cands: &mut Vec<&Sig>, score: &dyn Fn(&str, &str) -> bool| {
        let count = |s: &&Sig| {
            params(s)
                .iter()
                .zip(args)
                .filter(|(p, a)| score(p, a))
                .count()
        };
        let best = cands.iter().map(count).max().unwrap_or(0);
        cands.retain(|s| count(s) == best);
    };
    keep_best(&mut cands, &|p, a| p == a);
    keep_best(&mut cands, &|p, a| {
        p == a
            || matches!(
                (cats.get(p), cats.get(a)),
                (Some((pc, true)), Some((ac, _))) if pc == ac
            )
    });
    Some(cands)
}

/// The declared argument types of the overload `func_select_candidate`
/// picks when some arguments are untyped literals (`unknown`), for printing
/// the analysed call (`to_char(d, 'YYYY')` over a date resolves to
/// `to_char(timestamptz, text)`). Typed positions are narrowed as
/// [`select_candidates`] does; then each unknown position takes the string
/// category if any candidate offers it there, else the one category every
/// candidate agrees on, keeping a preferred type where one is offered.
/// The declared types of an `n`-argument call of a function whose one
/// variadic overload ends in `"any"` (`concat`, `concat_ws`, `format`): the
/// fixed parameters' types, then `any` for each variadic argument. `None`
/// for any other function.
pub(crate) fn variadic_any_args(name: &str, n: usize) -> Option<Vec<String>> {
    let mut candidates = sigs()
        .get(name)?
        .iter()
        .filter(|s| s.variadic && s.args.last().is_some_and(|a| a == "any"));
    let s = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    let fixed = s.args.len() - 1;
    if n < fixed {
        return None;
    }
    Some(
        (0..n)
            .map(|i| {
                if i < fixed {
                    s.args[i].clone()
                } else {
                    "any".into()
                }
            })
            .collect(),
    )
}

/// `None` unless exactly one non-variadic, non-polymorphic overload is left.
/// The concrete parameter types of the one polymorphic overload a call of
/// constants reaches, the polymorphic ones fixed by its typed arguments
/// (`enforce_generic_type_consistency`): `array_append('{1}', 2)` reads its
/// literal as `int4[]`. With no typed argument, the `anycompatible` family
/// reads every literal as `text` (`array_cat('{1}', '{2}')` is `text[]`).
/// Only the element / array / non-array families; `None` when typed
/// arguments disagree, which needs PostgreSQL's common-type rules.
pub(crate) fn polymorphic_args(name: &str, args: &[String]) -> Option<Vec<String>> {
    if crate::scalar::is_catalog_reader(name) || name.starts_with("pg_") || args.is_empty() {
        return None;
    }
    let cats = categories();
    if args
        .iter()
        .any(|a| a != "unknown" && !cats.contains_key(a.as_str()))
    {
        return None;
    }
    let overloads = sigs().get(name)?;
    let cands: Vec<&Sig> = overloads
        .iter()
        .filter(|s| !s.variadic && s.args.len() == args.len() && sig_accepts(s, args))
        .collect();
    let [only] = cands.as_slice() else {
        return None;
    };
    let scalar = |p: &str| {
        matches!(
            p,
            "anyelement" | "anynonarray" | "anycompatible" | "anycompatiblenonarray"
        )
    };
    let array = |p: &str| matches!(p, "anyarray" | "anycompatiblearray");
    if only
        .args
        .iter()
        .any(|p| p.starts_with("any") && p != "any" && !scalar(p) && !array(p))
    {
        return None;
    }
    let mut element: Option<String> = None;
    for (p, a) in only.args.iter().zip(args) {
        if a == "unknown" {
            continue;
        }
        let e = if array(p) {
            a.strip_suffix("[]")?.to_string()
        } else if scalar(p) {
            a.clone()
        } else {
            continue;
        };
        match &element {
            Some(have) if *have != e => return None,
            _ => element = Some(e),
        }
    }
    let compatible = only.args.iter().any(|p| p.starts_with("anycompatible"));
    let element = match element {
        Some(e) => e,
        None if compatible => "text".to_string(),
        None => return None,
    };
    if !only.args.iter().any(|p| scalar(p) || array(p)) {
        return None;
    }
    Some(
        only.args
            .iter()
            .map(|p| {
                if array(p) {
                    format!("{element}[]")
                } else if scalar(p) {
                    element.clone()
                } else {
                    p.clone()
                }
            })
            .collect(),
    )
}

pub(crate) fn resolved_args(name: &str, args: &[String]) -> Option<Vec<String>> {
    resolved_sig(name, args).map(|s| s.args.clone())
}

/// The concrete, non-set result type of [`resolved_args`]'s overload.
pub(crate) fn resolved_result_type(name: &str, args: &[String]) -> Option<String> {
    let s = resolved_sig(name, args)?;
    if s.retset
        || s.ret.starts_with("any")
        || matches!(s.ret.as_str(), "record" | "internal" | "void" | "trigger")
    {
        return None;
    }
    Some(s.ret.clone())
}

fn resolved_sig(name: &str, args: &[String]) -> Option<&'static Sig> {
    unknown_resolution(name, args)?.ok()
}

/// A call with untyped arguments that `func_select_candidate` cannot
/// settle: several overloads remain, or the unknown positions' categories
/// conflict. PostgreSQL's 42725 `function f(unknown) is not unique`.
/// PostgreSQL's 42804 `could not determine polymorphic type because input
/// has type unknown`: the one overload a call of constants can reach takes a
/// polymorphic parameter (`anyarray`, `anyelement`, ..., not the
/// `anycompatible` family) and every argument
/// in such a position is an untyped literal, so nothing fixes the type
/// (`array_length('{1,2,3}', 1)`, measured on 15.19).
pub(crate) fn unknown_polymorphic(name: &str, args: &[String]) -> bool {
    if crate::scalar::is_catalog_reader(name) || name.starts_with("pg_") || args.is_empty() {
        return false;
    }
    let cats = categories();
    if args
        .iter()
        .any(|a| a != "unknown" && !cats.contains_key(a.as_str()))
    {
        return false;
    }
    let Some(overloads) = sigs().get(name) else {
        return false;
    };
    let cands: Vec<&Sig> = overloads
        .iter()
        .filter(|s| !s.variadic && s.args.len() == args.len() && sig_accepts(s, args))
        .collect();
    let [only] = cands.as_slice() else {
        return false;
    };
    // The `anycompatible` family reads an untyped literal as `text`
    // instead (`array_cat('{1}', '{2}')` is `{1,2}`); `any` is no
    // polymorphic type at all.
    let polymorphic =
        |p: &str| p.starts_with("any") && p != "any" && !p.starts_with("anycompatible");
    let positions: Vec<usize> = only
        .args
        .iter()
        .enumerate()
        .filter(|(_, p)| polymorphic(p))
        .map(|(i, _)| i)
        .collect();
    !positions.is_empty() && positions.iter().all(|&i| args[i] == "unknown")
}

pub(crate) fn unknown_ambiguous(name: &str, args: &[String]) -> bool {
    matches!(unknown_resolution(name, args), Some(Err(())))
}

/// `resolved_sig`'s work: `Some(Ok)` one overload, `Some(Err)` ambiguous,
/// `None` when the rules here do not decide (no candidate, a polymorphic or
/// internal parameter, a type with no category).
fn unknown_resolution(
    name: &str,
    args: &[String],
) -> Option<std::result::Result<&'static Sig, ()>> {
    if crate::scalar::is_catalog_reader(name) || name.starts_with("pg_") || args.is_empty() {
        return None;
    }
    let cats = categories();
    if args
        .iter()
        .any(|a| a != "unknown" && !cats.contains_key(a.as_str()))
    {
        return None;
    }
    let overloads = sigs().get(name)?;
    let mut cands: Vec<&Sig> = overloads
        .iter()
        .filter(|s| !s.variadic && s.args.len() == args.len() && sig_accepts(s, args))
        .collect();
    // A polymorphic candidate is judged only when every argument is
    // untyped, and only for ambiguity: its parameters' category is the
    // pseudo-types' `P`, so `unnest('{1,2}')` (anyarray, anymultirange,
    // tsvector) has candidates of two categories and is 42725, while one
    // left standing would need the polymorphic rules, not modelled here.
    let polymorphic = cands
        .iter()
        .flat_map(|s| s.args.iter())
        .any(|p| p.starts_with("any") || matches!(p.as_str(), "record" | "internal" | "cstring"));
    if polymorphic && !args.iter().all(|a| a == "unknown") {
        return None;
    }
    let keep_best = |cands: &mut Vec<&Sig>, score: &dyn Fn(&str, &str) -> bool| {
        let count = |s: &&Sig| {
            s.args
                .iter()
                .zip(args)
                .filter(|(p, a)| *a != "unknown" && score(p, a))
                .count()
        };
        let best = cands.iter().map(count).max().unwrap_or(0);
        cands.retain(|s| count(s) == best);
    };
    keep_best(&mut cands, &|p, a| p == a);
    keep_best(&mut cands, &|p, a| {
        p == a
            || matches!(
                (cats.get(p), cats.get(a)),
                (Some((pc, true)), Some((ac, _))) if pc == ac
            )
    });
    // The unknown positions, as `func_select_candidate` takes them: a
    // category for EACH position over all the remaining candidates (a
    // string category wins a conflict, any other conflict leaves the
    // unknowns unresolved), and only then the candidates that agree at
    // every position -- kept if any do. Deciding one position at a time
    // let `ts_headline('english', 'a b c', 'b')` settle on one overload
    // where PostgreSQL 15 finds none agreeing at all three (42725).
    if cands.len() > 1 {
        let mut slots: Vec<(usize, char, bool)> = Vec::new();
        let mut resolved = true;
        for (i, a) in args.iter().enumerate() {
            if a != "unknown" {
                continue;
            }
            let mut slot: Option<(char, bool)> = None;
            let mut conflict = false;
            for s in &cands {
                let (c, p) = *cats.get(s.args[i].as_str())?;
                match slot {
                    None => slot = Some((c, p)),
                    Some((sc, sp)) if sc == c => slot = Some((sc, sp || p)),
                    Some(_) if c == 'S' => slot = Some((c, p)),
                    Some(_) => conflict = true,
                }
            }
            let (c, p) = slot?;
            if conflict && c != 'S' {
                resolved = false;
                break;
            }
            slots.push((i, c, p));
        }
        if resolved {
            let keep: Vec<&Sig> = cands
                .iter()
                .copied()
                .filter(|s| {
                    slots.iter().all(|&(i, c, p)| {
                        cats.get(s.args[i].as_str())
                            .is_some_and(|&(sc, sp)| sc == c && (!p || sp))
                    })
                })
                .collect();
            if !keep.is_empty() {
                cands = keep;
            }
        }
    }
    // Last gasp: the known arguments all of one type, with the unknowns
    // taken as that type, may leave exactly one candidate.
    if cands.len() > 1 {
        let mut known = args.iter().filter(|a| *a != "unknown");
        if let Some(first) = known.next() {
            if known.all(|k| k == first) {
                let as_known: Vec<String> = args.iter().map(|_| first.clone()).collect();
                let fits: Vec<&Sig> = cands
                    .iter()
                    .copied()
                    .filter(|s| sig_accepts(s, &as_known))
                    .collect();
                if let [one] = fits.as_slice() {
                    cands = vec![*one];
                }
            }
        }
    }
    match cands.as_slice() {
        [_, _, ..] => Some(Err(())),
        _ if polymorphic => None,
        [s] => Some(Ok(*s)),
        [] => None,
    }
}

/// Whether some overload of `name` declares exactly `args` (so a call
/// prints with no casts).
pub(crate) fn has_exact(name: &str, args: &[String]) -> bool {
    sigs().get(name).is_some_and(|o| {
        o.iter().any(|s| {
            !s.variadic
                && s.args.len() == args.len()
                && s.args.iter().zip(args).all(|(p, a)| p == a)
        })
    })
}

/// Does a built-in overload of `name` win a call of `args` over a user
/// function of that name with `user` parameters? PostgreSQL considers both
/// (`func_select_candidate`): the candidate with more exact matches among
/// the known arguments wins, then the one with more string parameters for
/// the unknown literals, and on a tie the earlier schema on the path
/// (`catalog_first`: `pg_catalog` unless the path puts the user
/// function's schema before it). A user function that cannot take the
/// arguments never wins; one the built-ins cannot take always does.
pub(crate) fn builtin_beats(
    name: &str,
    args: &[String],
    user: &[String],
    catalog_first: bool,
) -> bool {
    let Some(overloads) = sigs().get(name) else {
        return false;
    };
    let fitting: Vec<&Sig> = overloads.iter().filter(|s| sig_accepts(s, args)).collect();
    if fitting.is_empty() {
        return false;
    }
    if user.len() != args.len() || !user.iter().zip(args).all(|(p, a)| accepts(p, a)) {
        return true;
    }
    let unknown = |a: &str| a.is_empty() || a == "unknown";
    let exact = |params: &[String]| {
        params
            .iter()
            .zip(args)
            .filter(|(p, a)| !unknown(a) && p == a)
            .count()
    };
    let stringy = |params: &[String]| {
        params
            .iter()
            .zip(args)
            .filter(|(p, a)| {
                unknown(a) && matches!(p.as_str(), "text" | "varchar" | "bpchar" | "name")
            })
            .count()
    };
    let best =
        |f: &dyn Fn(&[String]) -> usize| fitting.iter().map(|s| f(&s.args)).max().unwrap_or(0);
    let (b, u) = (best(&exact), exact(user));
    if b != u {
        return b > u;
    }
    let (bs, us) = (best(&stringy), stringy(user));
    if bs != us {
        return bs > us;
    }
    catalog_first
}

/// Several overloads take `args` equally well: 42725 `is not unique`.
pub(crate) fn ambiguous(name: &str, args: &[String]) -> bool {
    select_candidates(name, args).is_some_and(|c| c.len() > 1)
}

/// The concrete, non-set return type of the one overload
/// `func_select_candidate` picks for `args`.
pub(crate) fn selected_result_type(name: &str, args: &[String]) -> Option<String> {
    match select_candidates(name, args)?.as_slice() {
        [s] if !s.retset
            && !s.ret.starts_with("any")
            && !matches!(
                s.ret.as_str(),
                "record" | "void" | "internal" | "trigger" | "unknown"
            ) =>
        {
            Some(s.ret.clone())
        }
        _ => None,
    }
}

fn sig_accepts(s: &Sig, args: &[String]) -> bool {
    {
        let n = args.len();
        let fixed = if s.variadic {
            s.args.len().saturating_sub(1)
        } else {
            s.args.len()
        };
        if !s.variadic && (n > s.args.len() || n + s.defaults < s.args.len()) {
            return false;
        }
        if s.variadic && n + s.defaults < fixed {
            return false;
        }
        args.iter().enumerate().all(|(i, a)| {
            let p = if i < fixed || !s.variadic {
                match s.args.get(i) {
                    Some(p) => p.as_str(),
                    None => return false,
                }
            } else {
                // Past the fixed arguments: the variadic array's element.
                let v = s.args.last().map(String::as_str).unwrap_or("any");
                if v == "any" {
                    "any"
                } else {
                    v.strip_suffix("[]").unwrap_or(v)
                }
            };
            accepts(p, a)
        })
    }
}

/// The declared argument types of the overload a call of `name` with
/// `args` resolves to: the one `func_select_candidate` picks, or else the
/// only overload that takes them. `None` when that is not determined here.
pub(crate) fn selected_args(name: &str, args: &[String]) -> Option<Vec<String>> {
    let params = |s: &Sig| -> Vec<String> {
        (0..args.len())
            .map(|i| param_at(s, i).unwrap_or("any").to_string())
            .collect()
    };
    if let Some(c) = select_candidates(name, args) {
        return match c.as_slice() {
            [s] if !s.variadic => Some(s.args.clone()),
            [s] => Some(params(s)),
            _ => None,
        };
    }
    let overloads = sigs().get(name)?;
    let mut takes = overloads.iter().filter(|s| sig_accepts(s, args));
    match (takes.next(), takes.next()) {
        (Some(s), None) if !s.variadic => Some(s.args.clone()),
        _ => None,
    }
}

/// Does every overload of the built-in `name` return a set
/// (`generate_series`, `unnest`, `json_each`)?
pub(crate) fn always_returns_set(name: &str) -> bool {
    sigs()
        .get(name)
        .is_some_and(|o| !o.is_empty() && o.iter().all(|s| s.retset))
}

/// Is `name` a PostgreSQL built-in function (any overload)?
pub fn is_builtin_function_name(name: &str) -> bool {
    sigs().contains_key(name)
}

/// The result type of the built-in binary operator `op` over exactly `l`
/// and `r`, each as `pg_type` names it.
pub fn binary_operator_result(op: &str, l: &str, r: &str) -> Option<String> {
    OPERATORS.lines().find_map(|line| {
        let mut it = line.split('\t');
        (it.next()? == op && it.next()? == l && it.next()? == r)
            .then(|| it.next().map(planner_type))
            .flatten()
    })
}
