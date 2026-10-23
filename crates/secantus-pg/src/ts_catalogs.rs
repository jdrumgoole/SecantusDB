//! The text-search catalogs (`pg_ts_config`, `pg_ts_dict`, `pg_ts_parser`,
//! `pg_ts_template`, `pg_ts_config_map`) as virtual tables over PostgreSQL
//! 15.19's rows (`secantus_pgplan::ts_catalog`). psql's `\dF` / `\dFd` /
//! `\dFp` read them, joined on the oids `regconfig` and `regdictionary`
//! carry.

use bson::{Bson, Document};
use secantus_pgcatalog::{Column, TableDef};

/// Each column's type, as `pg_attribute` gives it on 15.19.
fn column_type(column: &str) -> &'static str {
    match column {
        "cfgname" | "dictname" | "prsname" | "tmplname" => "name",
        "dictinitoption" => "text",
        "maptokentype" | "mapseqno" => "int4",
        "prsstart" | "prstoken" | "prsend" | "prsheadline" | "prslextype" | "tmplinit"
        | "tmpllexize" => "regproc",
        _ => "oid",
    }
}

/// The table definition of one text-search catalog, or `None`.
pub(crate) fn def(name: &str) -> Option<TableDef> {
    let catalog = secantus_pgplan::ts_catalog::catalog(name)?;
    Some(TableDef::new(
        name,
        catalog
            .columns
            .iter()
            .map(|c| Column::new(c, column_type(c), false))
            .collect(),
    ))
}

/// The rows of one text-search catalog.
pub(crate) fn rows(name: &str, def: &TableDef) -> Vec<Document> {
    let Some(catalog) = secantus_pgplan::ts_catalog::catalog(name) else {
        return Vec::new();
    };
    // A built-in configuration the database changed or dropped: its own
    // row (among the user rows) stands for it, or nothing does.
    let config_column = match name {
        "pg_ts_config" => catalog.columns.iter().position(|c| *c == "oid"),
        "pg_ts_config_map" => catalog.columns.iter().position(|c| *c == "mapcfg"),
        _ => None,
    };
    catalog
        .rows
        .iter()
        .filter(|row| {
            config_column
                .and_then(|i| row.get(i).copied().flatten())
                .and_then(|v| v.parse::<i64>().ok())
                .is_none_or(|oid| !secantus_pgplan::ts_catalog::builtin_hidden(oid))
        })
        .map(|row| {
            let mut d = Document::new();
            for (column, value) in catalog.columns.iter().zip(row) {
                let Some(field) = def.field_of(column) else {
                    continue;
                };
                let v = match (column_type(column), value) {
                    (_, None) => Bson::Null,
                    ("name" | "text", Some(v)) => Bson::String((*v).to_string()),
                    ("int4", Some(v)) => v.parse().map_or(Bson::Null, Bson::Int32),
                    ("regproc", Some(v)) => v.parse().map_or(Bson::Null, |oid| {
                        secantus_pgplan::regobj::value("regproc", oid)
                    }),
                    (_, Some(v)) => v.parse().map_or(Bson::Null, Bson::Int64),
                };
                d.insert(field, v);
            }
            d
        })
        .collect()
}

/// The rows of `pg_ts_config` and `pg_ts_config_map` for the
/// configurations `CREATE TEXT SEARCH CONFIGURATION` made.
pub(crate) fn user_rows(
    name: &str,
    def: &TableDef,
    configs: &[secantus_pgplan::ts_catalog::UserConfig],
    role_oid: impl Fn(&str) -> i64,
) -> Vec<Document> {
    let f = |c: &str| def.field_of(c);
    let mut out = Vec::new();
    for c in configs {
        match name {
            "pg_ts_config" => {
                let mut d = Document::new();
                let ns = secantus_pgplan::regobj::resolve("regnamespace", &c.schema).unwrap_or(0);
                for (column, value) in [
                    ("oid", Bson::Int64(c.oid)),
                    ("cfgname", Bson::String(c.name.clone())),
                    ("cfgnamespace", Bson::Int64(ns)),
                    ("cfgowner", Bson::Int64(role_oid(&c.owner))),
                    ("cfgparser", Bson::Int64(c.parser)),
                ] {
                    if let Some(field) = f(column) {
                        d.insert(field, value);
                    }
                }
                out.push(d);
            }
            "pg_ts_config_map" => {
                for (t, seq, dict) in &c.map {
                    let mut d = Document::new();
                    for (column, value) in [
                        ("mapcfg", Bson::Int64(c.oid)),
                        ("maptokentype", Bson::Int32(*t)),
                        ("mapseqno", Bson::Int32(*seq)),
                        ("mapdict", Bson::Int64(*dict)),
                    ] {
                        if let Some(field) = f(column) {
                            d.insert(field, value);
                        }
                    }
                    out.push(d);
                }
            }
            _ => {}
        }
    }
    out
}
