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
    catalog
        .rows
        .iter()
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
