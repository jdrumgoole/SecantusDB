//! PostgreSQL 15's text-search catalogs: `pg_ts_config`, `pg_ts_dict`,
//! `pg_ts_parser`, `pg_ts_template` and `pg_ts_config_map`, as `initdb`
//! leaves them (dumped from 15.19, `pg_ts_catalog.tsv`).
//!
//! They name what `regconfig` and `regdictionary` resolve to: `3748` is the
//! configuration `simple`, `'english'::regconfig::oid` is 13636. The server
//! has no `CREATE TEXT SEARCH CONFIGURATION`, so these rows are the whole
//! catalog.

const DATA: &str = include_str!("pg_ts_catalog.tsv");

/// One catalog: its columns and rows, each value as dumped (`\N` is NULL).
pub struct Catalog {
    pub columns: Vec<&'static str>,
    pub rows: Vec<Vec<Option<&'static str>>>,
}

/// The catalog named `name` (`pg_ts_config`, ...), or `None`.
pub fn catalog(name: &str) -> Option<Catalog> {
    let mut lines = DATA.lines().filter(|l| !l.starts_with('#'));
    let header = lines.by_ref().find(|l| {
        l.strip_prefix('[')
            .and_then(|r| r.split_once(']'))
            .is_some_and(|(n, _)| n == name)
    })?;
    let columns = header.split_once(']')?.1.split_whitespace().collect();
    let rows = lines
        .take_while(|l| !l.starts_with('['))
        .filter(|l| !l.is_empty())
        .map(|l| l.split('\t').map(|v| (v != "\\N").then_some(v)).collect())
        .collect();
    Some(Catalog { columns, rows })
}

/// The catalogs this module holds.
pub const NAMES: [&str; 5] = [
    "pg_ts_config",
    "pg_ts_dict",
    "pg_ts_parser",
    "pg_ts_template",
    "pg_ts_config_map",
];

/// `(oid, name)` of each row of `pg_ts_config` (`dict` false) or
/// `pg_ts_dict` (`dict` true).
fn names(dict: bool) -> Vec<(i64, &'static str)> {
    catalog(if dict { "pg_ts_dict" } else { "pg_ts_config" })
        .map(|c| {
            c.rows
                .iter()
                .filter_map(|r| Some(((*r.first()?)?.parse().ok()?, (*r.get(1)?)?)))
                .collect()
        })
        .unwrap_or_default()
}

/// A configuration's (or, with `dict`, a dictionary's) name by oid.
pub fn name_of(oid: i64, dict: bool) -> Option<&'static str> {
    names(dict)
        .into_iter()
        .find(|(o, _)| *o == oid)
        .map(|(_, n)| n)
}

/// A configuration's (or dictionary's) oid by name: an identifier, so
/// unquoted is folded to lower case, and `pg_catalog.` may qualify it.
pub fn oid_of(input: &str, dict: bool) -> Option<i64> {
    let t = input.trim();
    let t = t.strip_prefix("pg_catalog.").unwrap_or(t);
    let name = match t.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => t.to_ascii_lowercase(),
    };
    names(dict)
        .into_iter()
        .find(|(_, n)| *n == name)
        .map(|(o, _)| o)
}
