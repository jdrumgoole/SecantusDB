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

struct Sig {
    args: Vec<String>,
    variadic: bool,
    defaults: usize,
    ret: String,
    retset: bool,
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
                    args.split(',').map(str::to_string).collect()
                },
                variadic: v == "1",
                defaults: d.parse().unwrap_or(0),
                ret: ret.to_string(),
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
                Some((a.to_string(), b.to_string()))
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
                Some((name.to_string(), (cat, preferred)))
            })
            .collect()
    })
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
