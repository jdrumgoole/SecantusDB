//! The object-identifier types beyond `regtype` / `regclass`:
//! `regnamespace`, `regrole`, `regproc` and `regprocedure`. Each is an oid
//! that renders as its object's name, so a value is a tagged document holding
//! the oid (as `regclass` is), compared and filtered by that oid, and cast to
//! text as the name. The names come from tables the executor publishes per
//! statement. Measured against PostgreSQL 14.

use bson::{Bson, Document};

use crate::{Error, Result};

/// `(type name, document key)`.
pub const KINDS: &[(&str, &str)] = &[
    ("regnamespace", "__regnamespace_oid"),
    ("regrole", "__regrole_oid"),
    ("regproc", "__regproc_oid"),
    ("regprocedure", "__regprocedure_oid"),
    ("regcollation", "__regcollation_oid"),
];

thread_local! {
    static NAMESPACES: std::cell::RefCell<Vec<(String, i64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static ROLES: std::cell::RefCell<Vec<(String, i64)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// `(name, oid, argument types as regprocedure prints them)`.
    static PROCS: std::cell::RefCell<Vec<(String, i64, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub fn set_namespaces(v: Vec<(String, i64)>) {
    NAMESPACES.with(|t| *t.borrow_mut() = v);
}
/// `pg_get_userbyid(oid)`: the role's name, or PostgreSQL's
/// `unknown (OID=n)` for an oid no role has.
pub fn role_name(oid: i64) -> String {
    ROLES.with(|t| {
        t.borrow()
            .iter()
            .find(|(_, o)| *o == oid)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| match oid {
                // The built-in role that owns `public` from PostgreSQL 15 on.
                6171 => "pg_database_owner".to_string(),
                _ => format!("unknown (OID={oid})"),
            })
    })
}

/// Whether a role of this name exists (the published role catalog).
pub fn role_known(name: &str) -> bool {
    ROLES.with(|t| t.borrow().iter().any(|(n, _)| n == name))
}

pub fn set_roles(v: Vec<(String, i64)>) {
    ROLES.with(|t| *t.borrow_mut() = v);
}
pub fn set_procs(v: Vec<(String, i64, String)>) {
    PROCS.with(|t| *t.borrow_mut() = v);
}

/// PostgreSQL 15's own functions, `oid -> name` (`pg_proc_oids.tsv`, dumped
/// from `pg_proc where oid < 16384`): what a `regproc` of a built-in name
/// resolves to, and how one renders. pgjdbc's type cache compares
/// `typinput = 'pg_catalog.array_in'::regproc`.
const BUILTIN_PROCS: &str = include_str!("pg_proc_oids.tsv");

/// `(oid -> name, name -> oids)` over the built-in functions.
type BuiltinProcs = (
    std::collections::HashMap<i64, &'static str>,
    std::collections::HashMap<&'static str, Vec<i64>>,
);

fn builtin_procs() -> &'static BuiltinProcs {
    static T: std::sync::OnceLock<BuiltinProcs> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut by_oid = std::collections::HashMap::new();
        let mut by_name: std::collections::HashMap<&'static str, Vec<i64>> =
            std::collections::HashMap::new();
        for line in BUILTIN_PROCS.lines() {
            let Some((oid, name)) = line.split_once('\t') else {
                continue;
            };
            let Ok(oid) = oid.parse::<i64>() else {
                continue;
            };
            by_oid.insert(oid, name);
            by_name.entry(name).or_default().push(oid);
        }
        (by_oid, by_name)
    })
}

/// A built-in function's name by oid.
pub fn builtin_proc_name(oid: i64) -> Option<&'static str> {
    builtin_procs().0.get(&oid).copied()
}

/// The oids of the built-in functions named `name`.
pub fn builtin_proc_oids(name: &str) -> &'static [i64] {
    builtin_procs().1.get(name).map_or(&[], Vec::as_slice)
}

/// A `regproc` value for the built-in function named `name` (`-` is 0),
/// when exactly one has that name.
pub fn builtin_regproc(name: &str) -> Option<Bson> {
    if name == "-" {
        return Some(value("regproc", 0));
    }
    match builtin_proc_oids(name) {
        [one] => Some(value("regproc", *one)),
        _ => None,
    }
}

pub fn is_kind(t: &str) -> bool {
    KINDS.iter().any(|(k, _)| *k == t)
}

pub fn value(kind: &str, oid: i64) -> Bson {
    let key = KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("__regnamespace_oid", |(_, key)| *key);
    let mut d = Document::new();
    d.insert(key, Bson::Int64(oid));
    Bson::Document(d)
}

/// `(kind, oid)` of a value of one of these types.
pub fn from_bson(v: &Bson) -> Option<(&'static str, i64)> {
    let Bson::Document(d) = v else { return None };
    if d.len() != 1 {
        return None;
    }
    KINDS
        .iter()
        .find_map(|(kind, key)| d.get_i64(key).ok().map(|oid| (*kind, oid)))
}

/// The name an oid of `kind` renders as; an oid naming nothing prints as
/// the number, and 0 as `-`.
pub fn text(kind: &str, oid: i64) -> String {
    if oid == 0 {
        return "-".into();
    }
    let found = match kind {
        "regnamespace" => NAMESPACES.with(|t| {
            t.borrow()
                .iter()
                .find(|(_, o)| *o == oid)
                .map(|(n, _)| crate::scalar::quote_identifier(n))
        }),
        "regrole" => ROLES.with(|t| {
            t.borrow()
                .iter()
                .find(|(_, o)| *o == oid)
                .map(|(n, _)| crate::scalar::quote_identifier(n))
        }),
        "regcollation" => collation_names()
            .into_iter()
            .find(|(_, o)| *o == oid)
            .map(|(n, _)| crate::scalar::quote_identifier(&n)),
        "regproc" => PROCS
            .with(|t| {
                t.borrow()
                    .iter()
                    .find(|(_, o, _)| *o == oid)
                    .map(|(n, _, _)| n.clone())
            })
            .or_else(|| builtin_proc_name(oid).map(str::to_string)),
        _ => PROCS.with(|t| {
            t.borrow()
                .iter()
                .find(|(_, o, _)| *o == oid)
                .map(|(n, _, args)| format!("{n}({args})"))
        }),
    };
    found.unwrap_or_else(|| oid.to_string())
}

fn unquote(name: &str) -> String {
    let t = name.trim();
    match t.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => t.to_ascii_lowercase(),
    }
}

/// The oid a name of `kind` resolves to (a number stands for itself).
pub fn resolve(kind: &str, input: &str) -> Result<i64> {
    if let Ok(oid) = input.trim().parse::<i64>() {
        return Ok(oid);
    }
    match kind {
        "regnamespace" => {
            let name = unquote(input);
            NAMESPACES
                .with(|t| t.borrow().iter().find(|(n, _)| *n == name).map(|(_, o)| *o))
                .ok_or_else(|| {
                    Error::Sqlstate("3F000", format!("schema \"{name}\" does not exist"))
                })
        }
        "regcollation" => {
            let name = unquote(input);
            collation_names()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, o)| o)
                .ok_or_else(|| {
                    Error::UndefinedObject(format!(
                        "collation \"{name}\" for encoding \"UTF8\" does not exist"
                    ))
                })
        }
        "regrole" => {
            let name = unquote(input);
            ROLES
                .with(|t| t.borrow().iter().find(|(n, _)| *n == name).map(|(_, o)| *o))
                .ok_or_else(|| Error::UndefinedObject(format!("role \"{name}\" does not exist")))
        }
        "regproc" => {
            let mut name = unquote(input);
            // `pg_catalog.array_in`: the built-ins live in pg_catalog.
            let qualified_builtin = match name.strip_prefix("pg_catalog.") {
                Some(bare) => {
                    name = bare.to_string();
                    true
                }
                None => false,
            };
            let mut hits: Vec<i64> = if qualified_builtin {
                Vec::new()
            } else {
                PROCS.with(|t| {
                    t.borrow()
                        .iter()
                        .filter(|(n, _, _)| *n == name)
                        .map(|(_, o, _)| *o)
                        .collect()
                })
            };
            if hits.is_empty() {
                hits = builtin_proc_oids(&name).to_vec();
            }
            match hits.as_slice() {
                [one] => Ok(*one),
                [] => Err(Error::UndefinedFunction(format!(
                    "function \"{name}\" does not exist"
                ))),
                _ => Err(Error::Sqlstate(
                    "42725",
                    format!("more than one function named \"{name}\""),
                )),
            }
        }
        _ => {
            // `name(argtypes)`.
            let (name, args) = input
                .trim()
                .split_once('(')
                .map(|(n, a)| (unquote(n), a.trim_end_matches(')').trim().to_string()))
                .ok_or_else(|| {
                    Error::Sqlstate(
                        "22P02",
                        format!("invalid input syntax for type regprocedure: \"{input}\""),
                    )
                })?;
            let wanted: Vec<String> = args
                .split(',')
                .map(|a| {
                    let a = a.trim();
                    // Any spelling of a type (`int`, `integer`) as its
                    // canonical name, then as PostgreSQL displays it.
                    let canonical = crate::pgtypes::oid_of_name(a)
                        .and_then(crate::pgtypes::name_of_oid)
                        .map_or_else(|| a.to_string(), str::to_string);
                    crate::display_type(&canonical)
                })
                .filter(|a| !a.is_empty())
                .collect();
            PROCS
                .with(|t| {
                    t.borrow()
                        .iter()
                        .find(|(n, _, a)| {
                            *n == name
                                && a.split(',')
                                    .map(str::trim)
                                    .filter(|x| !x.is_empty())
                                    .collect::<Vec<_>>()
                                    == wanted.iter().map(String::as_str).collect::<Vec<_>>()
                        })
                        .map(|(_, o, _)| *o)
                })
                .ok_or_else(|| {
                    Error::UndefinedFunction(format!("function \"{input}\" does not exist"))
                })
        }
    }
}

/// A cast involving one of these types: from its value, or to it.
pub fn cast(value: &Bson, target: &str) -> Option<Result<Bson>> {
    if let Some((kind, oid)) = from_bson(value) {
        return Some(match target {
            t if t == kind => Ok(value.clone()),
            "text" | "varchar" | "name" | "bpchar" => Ok(Bson::String(text(kind, oid))),
            "int4" | "int8" | "oid" | "integer" | "int" | "bigint" => Ok(Bson::Int64(oid)),
            t if is_kind(t) => Ok(self::value(t, oid)),
            _ => Err(Error::CannotCoerce(format!(
                "cannot cast type {kind} to {}",
                crate::display_type(target)
            ))),
        });
    }
    if !is_kind(target) {
        return None;
    }
    Some(match value {
        Bson::Int32(o) => Ok(self::value(target, i64::from(*o))),
        Bson::Int64(o) => Ok(self::value(target, *o)),
        Bson::String(s) => resolve(target, s).map(|o| self::value(target, o)),
        other => Err(Error::CannotCoerce(format!(
            "cannot cast type {} to {target}",
            crate::display_type(crate::inferred_type(other))
        ))),
    })
}

/// Every collation by name and oid: PostgreSQL's fixed ones, then the
/// database's own.
pub fn collation_names() -> Vec<(String, i64)> {
    let mut out: Vec<(String, i64)> = [
        ("default", 100),
        ("C", 950),
        ("POSIX", 951),
        ("ucs_basic", 962),
        ("und-x-icu", 12713),
        ("en-x-icu", 12860),
    ]
    .into_iter()
    .map(|(n, o)| (n.to_string(), o))
    .collect();
    out.extend(
        crate::collation::user_collations()
            .into_iter()
            .map(|c| (c.name, c.oid)),
    );
    out
}
