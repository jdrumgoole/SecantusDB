//! The object-identifier types beyond `regtype` / `regclass`:
//! `regnamespace`, `regrole`, `regproc`, `regprocedure`, `regoper` and
//! `regoperator`. Each is an oid
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
    ("regconfig", "__regconfig_oid"),
    ("regdictionary", "__regdictionary_oid"),
    ("regoper", "__regoper_oid"),
    ("regoperator", "__regoperator_oid"),
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

/// A role's name by oid, or the oid as text for one no role has -- what an
/// `aclitem` prints for it (`{99999=arwdDxt/99999}` on PostgreSQL 15.19).
pub fn role_name_or_oid(oid: i64) -> String {
    if oid == 0 {
        return String::new();
    }
    ROLES
        .with(|t| {
            t.borrow()
                .iter()
                .find(|(_, o)| *o == oid)
                .map(|(n, _)| n.clone())
        })
        .unwrap_or_else(|| oid.to_string())
}

/// A role's oid by name: PUBLIC (the empty name) is 0, and a run of digits
/// no role is named is the oid `aclitemout` printed for a role it did not
/// find.
pub fn role_oid(name: &str) -> Option<i64> {
    if name.is_empty() {
        return Some(0);
    }
    ROLES
        .with(|t| t.borrow().iter().find(|(n, _)| n == name).map(|(_, o)| *o))
        .or_else(|| name.parse().ok())
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

/// Is `oid` one of PostgreSQL 15's own functions?
pub(crate) fn is_builtin_proc(oid: i64) -> bool {
    builtin_procs().0.contains_key(&oid)
}

/// Each built-in function as `regprocedure` prints it, by oid, read from
/// PostgreSQL 15.19 (`oid::regprocedure::text`): the name quoted where it
/// is a keyword there (`"char"(integer)`) and qualified outside
/// `pg_catalog` (`information_schema._pg_expandarray(anyarray)`).
const BUILTIN_REGPROCEDURES: &str = include_str!("pg_regprocedure.tsv");

/// `oid -> (printed form, bare name, argument types as printed)`.
type BuiltinRegprocedures =
    std::collections::HashMap<i64, (&'static str, String, Vec<&'static str>)>;

fn builtin_regprocedures() -> &'static BuiltinRegprocedures {
    static T: std::sync::OnceLock<BuiltinRegprocedures> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        BUILTIN_REGPROCEDURES
            .lines()
            .filter_map(|l| {
                let (oid, text) = l.split_once('\t')?;
                let (name, args) = text.split_once('(')?;
                let name = name.rsplit('.').next().unwrap_or(name);
                let args = args
                    .strip_suffix(')')?
                    .split(',')
                    .filter(|a| !a.is_empty())
                    .collect();
                Some((oid.parse().ok()?, (text, unquote(name), args)))
            })
            .collect()
    })
}

/// PostgreSQL 15.19's operators (`pg_operator`, oids below 16384): `(oid,
/// name, left, right)`, each operand as `format_type` prints it and `NONE`
/// for a prefix operator's missing left one.
const BUILTIN_OPERATORS: &str = include_str!("pg_operator_oids.tsv");

type BuiltinOperator = (i64, &'static str, &'static str, &'static str);

fn builtin_operators() -> &'static [BuiltinOperator] {
    static T: std::sync::OnceLock<Vec<BuiltinOperator>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        BUILTIN_OPERATORS
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                Some((it.next()?.parse().ok()?, it.next()?, it.next()?, it.next()?))
            })
            .collect()
    })
}

/// A type name in a `regprocedure` / `regoperator` input as `format_type`
/// prints it: `int` and `integer` are both `integer`.
fn display_arg(a: &str) -> String {
    let a = a.trim();
    // The quoted `"char"` is the one-byte type, a bare `char` is `character`.
    if a.trim_end_matches("[]") == crate::QUOTED_CHAR {
        return a.to_string();
    }
    let canonical = crate::pgtypes::oid_of_name(a)
        .and_then(crate::pgtypes::type_name_of_oid)
        .unwrap_or_else(|| a.to_string());
    crate::display_type(&canonical)
}

/// `name(args)` split as `parseNameAndArgTypes` reads it: PostgreSQL's
/// 22P02 `expected a left parenthesis` / `expected a right parenthesis`.
fn name_and_args(input: &str) -> Result<(String, Vec<String>)> {
    let t = input.trim();
    let (name, rest) = t
        .split_once('(')
        .ok_or_else(|| Error::Sqlstate("22P02", "expected a left parenthesis".into()))?;
    let args = rest
        .trim_end()
        .strip_suffix(')')
        .ok_or_else(|| Error::Sqlstate("22P02", "expected a right parenthesis".into()))?;
    let args = args
        .split(',')
        .map(display_arg)
        .filter(|a| !a.is_empty())
        .collect();
    Ok((unquote(name), args))
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
        "regconfig" => crate::ts_catalog::display_of(oid, false),
        "regdictionary" => crate::ts_catalog::display_of(oid, true),
        // `regprocout`: each part of the name quoted as an identifier needs
        // (`"RI_FKey_check_ins"`).
        "regproc" => PROCS
            .with(|t| {
                t.borrow()
                    .iter()
                    .find(|(_, o, _)| *o == oid)
                    .map(|(n, _, _)| {
                        let (schema, name) = crate::schemas::split_key(n);
                        // Qualified unless its schema is on the search path.
                        if !crate::schemas::search_path().contains(&schema) {
                            format!(
                                "{}.{}",
                                crate::scalar::quote_identifier(&schema),
                                crate::scalar::quote_identifier(&name)
                            )
                        } else {
                            crate::scalar::quote_identifier(&name)
                        }
                    })
            })
            .or_else(|| builtin_proc_name(oid).map(crate::scalar::quote_identifier)),
        // `regoperout`: the bare name when it alone finds this operator,
        // else qualified (`551::regoper` is `pg_catalog.+`).
        "regoper" => builtin_operators().iter().find(|o| o.0 == oid).map(|o| {
            let unique = builtin_operators().iter().filter(|x| x.1 == o.1).count() == 1;
            if unique {
                o.1.to_string()
            } else {
                format!("pg_catalog.{}", o.1)
            }
        }),
        "regoperator" => builtin_operators()
            .iter()
            .find(|o| o.0 == oid)
            .map(|o| format!("{}({},{})", o.1, o.2, o.3)),
        _ => PROCS
            .with(|t| {
                t.borrow()
                    .iter()
                    .find(|(_, o, _)| *o == oid)
                    .map(|(n, _, args)| format!("{n}({args})"))
            })
            .or_else(|| builtin_regprocedures().get(&oid).map(|p| p.0.to_string())),
    };
    found.unwrap_or_else(|| oid.to_string())
}

/// A schema or role name is ONE identifier: `a.b` is PostgreSQL's 42602
/// (`stringToQualifiedNameList` gives two names).
fn single_name(input: &str) -> Result<String> {
    let t = input.trim();
    let quoted = t.starts_with('"') && t.ends_with('"') && t.len() >= 2;
    if !quoted && t.contains('.') {
        return Err(Error::Sqlstate("42602", "invalid name syntax".into()));
    }
    Ok(unquote(t))
}

/// The `reg*` type a `to_reg*` function resolves to, which answers NULL
/// where the cast would say the object does not exist.
pub fn to_reg_kind(function: &str) -> Option<&'static str> {
    Some(match function {
        "to_regnamespace" => "regnamespace",
        "to_regrole" => "regrole",
        "to_regcollation" => "regcollation",
        "to_regproc" => "regproc",
        "to_regprocedure" => "regprocedure",
        _ => return None,
    })
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
            let name = single_name(input)?;
            NAMESPACES
                .with(|t| t.borrow().iter().find(|(n, _)| *n == name).map(|(_, o)| *o))
                .ok_or_else(|| {
                    Error::Sqlstate("3F000", format!("schema \"{name}\" does not exist"))
                })
        }
        "regconfig" | "regdictionary" => {
            let dict = kind == "regdictionary";
            crate::ts_catalog::oid_of(input, dict).ok_or_else(|| {
                Error::Sqlstate(
                    "42704",
                    format!(
                        "text search {} \"{}\" does not exist",
                        if dict { "dictionary" } else { "configuration" },
                        input.trim()
                    ),
                )
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
            let name = single_name(input)?;
            ROLES
                .with(|t| t.borrow().iter().find(|(n, _)| *n == name).map(|(_, o)| *o))
                .ok_or_else(|| Error::UndefinedObject(format!("role \"{name}\" does not exist")))
        }
        "regproc" => {
            // `stringToQualifiedNameList`: a quoted part keeps its case.
            let parts = crate::split_relation_name(input)?;
            // `pg_catalog.array_in`: the built-ins live in pg_catalog.
            let (name, qualified_builtin) = match parts.as_slice() {
                [schema, bare] if schema == "pg_catalog" => (bare.clone(), true),
                [schema, bare] => (crate::schemas::relation_key(schema, bare), false),
                _ => (parts.join("."), false),
            };
            let named = |key: &str| -> Vec<i64> {
                PROCS.with(|t| {
                    t.borrow()
                        .iter()
                        .filter(|(n, _, _)| n == key)
                        .map(|(_, o, _)| *o)
                        .collect()
                })
            };
            let mut hits: Vec<i64> = if qualified_builtin {
                Vec::new()
            } else if parts.len() == 1 {
                // A bare name: the first schema on the search path holding it.
                crate::schemas::search_path()
                    .iter()
                    .map(|s| named(&crate::schemas::relation_key(s, &name)))
                    .find(|h| !h.is_empty())
                    .unwrap_or_default()
            } else {
                named(&name)
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
        // `regoperin`: the one operator of that name (42883 for none,
        // 42725 for several -- `'+'::regoper`).
        "regoper" => {
            let name = input.trim();
            let name = name.strip_prefix("pg_catalog.").unwrap_or(name);
            let hits: Vec<i64> = builtin_operators()
                .iter()
                .filter(|o| o.1 == name)
                .map(|o| o.0)
                .collect();
            match hits.as_slice() {
                [one] => Ok(*one),
                [] => Err(Error::UndefinedFunction(format!(
                    "operator does not exist: {input}"
                ))),
                _ => Err(Error::Sqlstate(
                    "42725",
                    format!("more than one operator named {name}"),
                )),
            }
        }
        // `regoperatorin`: `name(left,right)`, exactly two types (`NONE`
        // for a prefix operator's left).
        "regoperator" => {
            let (name, args) = name_and_args(input)?;
            let name = name
                .strip_prefix("pg_catalog.")
                .unwrap_or(&name)
                .to_string();
            let args: Vec<String> = args
                .into_iter()
                .map(|a| {
                    if a.eq_ignore_ascii_case("none") {
                        "NONE".into()
                    } else {
                        a
                    }
                })
                .collect();
            match args.len() {
                2 => {}
                1 => return Err(Error::Sqlstate("42P02", "missing argument".into())),
                _ => return Err(Error::Sqlstate("54023", "too many arguments".into())),
            }
            builtin_operators()
                .iter()
                .find(|o| o.1 == name && o.2 == args[0] && o.3 == args[1])
                .map(|o| o.0)
                .ok_or_else(|| {
                    Error::UndefinedFunction(format!("operator does not exist: {input}"))
                })
        }
        _ => {
            // `name(argtypes)`, each type in any spelling (`int`, `integer`).
            let (name, wanted) = name_and_args(input)?;
            let bare = name.rsplit('.').next().unwrap_or(&name);
            let builtin = || {
                builtin_proc_oids(bare).iter().copied().find(|oid| {
                    builtin_regprocedures()
                        .get(oid)
                        .is_some_and(|p| p.2.iter().copied().eq(wanted.iter().map(String::as_str)))
                })
            };
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
                .or_else(builtin)
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
    ]
    .into_iter()
    .map(|(n, o)| (n.to_string(), o))
    .collect();
    out.extend(crate::collation::icu_collations().map(|(o, n, _)| (n.to_string(), o)));
    out.extend(
        crate::collation::user_collations()
            .into_iter()
            .map(|c| (c.name, c.oid)),
    );
    out
}
