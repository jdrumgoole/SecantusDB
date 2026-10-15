//! The procedural-language validators and `postgresql_fdw_validator`, as
//! PostgreSQL 15 has them (fmgr.c's `CheckFunctionValidatorAccess`,
//! pg_proc.c's `fmgr_*_validator`, foreign.c's `postgresql_fdw_validator`;
//! measured on 15.19).
//!
//! A validator is called with a function's oid. It refuses an oid naming no
//! function (42883) and a function of another language (42501, naming the
//! language's own validator); otherwise it answers `void`, the function's
//! body having been checked when it was created.

use super::*;

/// PostgreSQL 15's own functions whose language is not `internal`, as
/// `oid -> prolang` (13 `c`, 14 `sql`); every other built-in is internal.
const BUILTIN_LANGS: &str = include_str!("pg_proc_lang.tsv");

/// `pg_language`'s validators: internal, c, sql, plpgsql.
const VALIDATORS: &[(&str, i64)] = &[
    ("fmgr_internal_validator", 2246),
    ("fmgr_c_validator", 2247),
    ("fmgr_sql_validator", 2248),
    ("plpgsql_validator", 14034),
];

/// The `plpgsql` language's oid on this server (`pg_language`).
const PLPGSQL_LANGUAGE: i64 = 14078;

thread_local! {
    /// The user functions' `(oid, prolang)`, installed per statement by the
    /// wire layer with the functions themselves.
    static USER_FUNCTION_LANGS: std::cell::RefCell<std::collections::HashMap<i64, i64>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The user functions' languages by oid, as `pg_proc.prolang` has them.
pub fn set_user_function_languages(langs: Vec<(i64, i64)>) {
    USER_FUNCTION_LANGS.with(|t| *t.borrow_mut() = langs.into_iter().collect());
}

pub fn is_function(name: &str) -> bool {
    name == "postgresql_fdw_validator" || VALIDATORS.iter().any(|(n, _)| *n == name)
}

pub fn result_type(name: &str) -> Option<&'static str> {
    if name == "postgresql_fdw_validator" {
        Some("bool")
    } else if is_function(name) {
        Some("void")
    } else {
        None
    }
}

fn builtin_language(oid: i64) -> Option<i64> {
    static T: std::sync::OnceLock<std::collections::HashMap<i64, i64>> = std::sync::OnceLock::new();
    let langs = T.get_or_init(|| {
        BUILTIN_LANGS
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let (o, lang) = l.split_once('\t')?;
                Some((o.parse().ok()?, lang.trim().parse().ok()?))
            })
            .collect()
    });
    if let Some(lang) = langs.get(&oid) {
        return Some(*lang);
    }
    regobj::is_builtin_proc(oid).then_some(12)
}

fn language_validator(lang: i64) -> i64 {
    match lang {
        12 => 2246,
        13 => 2247,
        14 => 2248,
        PLPGSQL_LANGUAGE => 14034,
        _ => 0,
    }
}

fn oid_arg(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(i64::from(*i)),
        Bson::Int64(i) => Some(*i),
        other => regobj::from_bson(other).map(|(_, oid)| oid),
    }
}

/// `CheckFunctionValidatorAccess`, then the validator's own (passing) check.
fn validate(name: &str, function: i64) -> Result<Bson> {
    let mine = VALIDATORS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or(0, |(_, oid)| *oid);
    let lang = USER_FUNCTION_LANGS
        .with(|t| t.borrow().get(&function).copied())
        .or_else(|| builtin_language(function))
        .ok_or_else(|| {
            Error::UndefinedFunction(format!("function with OID {function} does not exist"))
        })?;
    let expected = language_validator(lang);
    if expected != mine {
        return Err(Error::Sqlstate(
            "42501",
            format!(
                "language validation function {mine} called for language {lang} instead of {expected}"
            ),
        ));
    }
    Ok(Bson::String(String::new()))
}

/// `postgresql_fdw_validator(options, catalog)`: the libpq options a
/// foreign server (`pg_foreign_server`, 1417) or a user mapping
/// (`pg_user_mapping`, 1418) may set, and none for any other catalog.
fn fdw_validate(options: &Bson, catalog: i64) -> Result<Bson> {
    const SERVER: &[&str] = &[
        "authtype",
        "service",
        "connect_timeout",
        "dbname",
        "host",
        "hostaddr",
        "port",
        "tty",
        "options",
        "requiressl",
        "sslmode",
        "gsslib",
    ];
    const MAPPING: &[&str] = &["user", "password"];
    let valid: &[&str] = match catalog {
        1417 => SERVER,
        1418 => MAPPING,
        _ => &[],
    };
    let items = match arrays::strip(options) {
        Bson::Array(items) => items,
        _ => Vec::new(),
    };
    for item in items.iter().filter(|i| **i != Bson::Null) {
        let text = value_text(item);
        let key = text.split_once('=').map_or(text.as_str(), |(k, _)| k);
        if !valid.contains(&key) {
            let hint = if valid.is_empty() {
                "There are no valid options in this context.".to_string()
            } else {
                format!("Valid options in this context are: {}", valid.join(", "))
            };
            return Err(Error::Sqlstate(
                "42601",
                format!("invalid option \"{key}\"\nHint: {hint}"),
            ));
        }
    }
    Ok(Bson::Boolean(true))
}

/// One of these functions; `None` when `name` is not one.
pub fn call(name: &str, args: &[Bson]) -> Option<Result<Bson>> {
    if !is_function(name) {
        return None;
    }
    if args.contains(&Bson::Null) {
        return Some(Ok(Bson::Null));
    }
    Some(match (name, args) {
        ("postgresql_fdw_validator", [options, catalog]) => match oid_arg(catalog) {
            Some(c) => fdw_validate(options, c),
            None => Err(Error::UndefinedFunction(format!(
                "function {name} does not exist"
            ))),
        },
        (_, [function]) => match oid_arg(function) {
            Some(oid) => validate(name, oid),
            None => Err(Error::UndefinedFunction(format!(
                "function {name} does not exist"
            ))),
        },
        _ => Err(Error::UndefinedFunction(format!(
            "function {name} does not exist"
        ))),
    })
}
