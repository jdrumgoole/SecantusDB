//! PostgreSQL's own relations as `information_schema` lists them: every
//! `pg_catalog` and `information_schema` table and view, and their columns,
//! measured from PostgreSQL 15.19 (`catalog_relations.tsv`,
//! `catalog_columns.tsv`). A client that browses the system schemas through
//! `information_schema` sees what it would see on PostgreSQL.

use bson::{Bson, Document};
use secantus_pgcatalog::TableDef;

const RELATIONS: &str = include_str!("catalog_relations.tsv");
const COLUMNS: &str = include_str!("catalog_columns.tsv");

fn cell(s: &str) -> Option<&str> {
    (s != "\\N").then_some(s)
}

fn text(s: &str) -> Bson {
    cell(s).map_or(Bson::Null, |v| Bson::String(v.to_string()))
}

fn number(s: &str) -> Bson {
    cell(s)
        .and_then(|v| v.parse::<i32>().ok())
        .map_or(Bson::Null, Bson::Int32)
}

/// `information_schema.tables` rows for the system schemas.
pub(crate) fn relation_rows(def: &TableDef, db: &str) -> Vec<Document> {
    let f = |name: &str| def.field_of(name).expect("column");
    RELATIONS
        .lines()
        .filter_map(|l| {
            let [schema, name, kind] = l.split('\t').collect::<Vec<_>>()[..] else {
                return None;
            };
            let mut d = Document::new();
            d.insert(f("table_catalog"), db);
            d.insert(f("table_schema"), schema);
            d.insert(f("table_name"), name);
            d.insert(f("table_type"), kind);
            Some(d)
        })
        .collect()
}

/// `information_schema.columns` rows for the system schemas.
pub(crate) fn column_rows(def: &TableDef, db: &str) -> Vec<Document> {
    let has = |name: &str| def.field_of(name);
    COLUMNS
        .lines()
        .filter_map(|l| {
            let v: Vec<&str> = l.split('\t').collect();
            let [schema, table, column, ordinal, default, nullable, data_type, charlen, precision, scale, dtprec, udt, identity, collation, generated, domain, udt_schema] =
                v[..]
            else {
                return None;
            };
            let mut d = Document::new();
            let mut put = |k: &str, b: Bson| {
                if let Some(field) = has(k) {
                    d.insert(field, b);
                }
            };
            put("table_catalog", Bson::String(db.to_string()));
            put("table_schema", text(schema));
            put("table_name", text(table));
            put("column_name", text(column));
            put("ordinal_position", number(ordinal));
            put("column_default", text(default));
            put("is_nullable", text(nullable));
            put("data_type", text(data_type));
            put("character_maximum_length", number(charlen));
            put("numeric_precision", number(precision));
            put("numeric_scale", number(scale));
            put("datetime_precision", number(dtprec));
            put("udt_catalog", Bson::String(db.to_string()));
            put("udt_schema", text(udt_schema));
            put("udt_name", text(udt));
            put("is_identity", text(identity));
            put("identity_generation", Bson::Null);
            put(
                "collation_catalog",
                if cell(collation).is_some() {
                    Bson::String(db.to_string())
                } else {
                    Bson::Null
                },
            );
            put(
                "collation_schema",
                if cell(collation).is_some() {
                    Bson::String("pg_catalog".into())
                } else {
                    Bson::Null
                },
            );
            put("collation_name", text(collation));
            put("domain_catalog", if cell(domain).is_some() { Bson::String(db.to_string()) } else { Bson::Null });
            put("domain_schema", if cell(domain).is_some() { Bson::String("information_schema".into()) } else { Bson::Null });
            put("domain_name", text(domain));
            put("is_generated", text(generated));
            put("generation_expression", Bson::Null);
            Some(d)
        })
        .collect()
}
