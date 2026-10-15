//! `information_schema.columns`: its 44 columns in PostgreSQL 15's order,
//! and the type facts each row derives from a column's type and modifier.
//!
//! The derivations transcribe `information_schema.sql`'s helper functions
//! (`_pg_char_max_length`, `_pg_char_octet_length`, `_pg_numeric_precision`,
//! `_pg_numeric_precision_radix`, `_pg_numeric_scale`,
//! `_pg_datetime_precision`, `_pg_interval_type`), measured on 15.19 over a
//! table of every kind of column.

use bson::{Bson, Document};
use secantus_pgcatalog::{Column, TableDef};

/// `(column, type)` in PostgreSQL's order. The view's domains
/// (`sql_identifier`, `cardinal_number`, `character_data`, `yes_or_no`) are
/// `name`, `int4` and `varchar` over the wire.
const COLUMNS: &[(&str, &str)] = &[
    ("table_catalog", "name"),
    ("table_schema", "name"),
    ("table_name", "name"),
    ("column_name", "name"),
    ("ordinal_position", "int4"),
    ("column_default", "varchar"),
    ("is_nullable", "varchar"),
    ("data_type", "varchar"),
    ("character_maximum_length", "int4"),
    ("character_octet_length", "int4"),
    ("numeric_precision", "int4"),
    ("numeric_precision_radix", "int4"),
    ("numeric_scale", "int4"),
    ("datetime_precision", "int4"),
    ("interval_type", "varchar"),
    ("interval_precision", "int4"),
    ("character_set_catalog", "name"),
    ("character_set_schema", "name"),
    ("character_set_name", "name"),
    ("collation_catalog", "name"),
    ("collation_schema", "name"),
    ("collation_name", "name"),
    ("domain_catalog", "name"),
    ("domain_schema", "name"),
    ("domain_name", "name"),
    ("udt_catalog", "name"),
    ("udt_schema", "name"),
    ("udt_name", "name"),
    ("scope_catalog", "name"),
    ("scope_schema", "name"),
    ("scope_name", "name"),
    ("maximum_cardinality", "int4"),
    ("dtd_identifier", "name"),
    ("is_self_referencing", "varchar"),
    ("is_identity", "varchar"),
    ("identity_generation", "varchar"),
    ("identity_start", "varchar"),
    ("identity_increment", "varchar"),
    ("identity_maximum", "varchar"),
    ("identity_minimum", "varchar"),
    ("identity_cycle", "varchar"),
    ("is_generated", "varchar"),
    ("generation_expression", "varchar"),
    ("is_updatable", "varchar"),
];

pub(crate) fn def() -> TableDef {
    TableDef::new(
        "columns",
        COLUMNS
            .iter()
            .map(|(n, t)| Column::new(n, t, false))
            .collect(),
    )
}

/// `pg_encoding_max_length` for the server encoding, UTF8.
const MAX_ENCODING_LENGTH: i32 = 4;

fn int(v: i32) -> Bson {
    Bson::Int32(v)
}

/// Set the columns derived from a column's type `pg_type` and modifier
/// `typmod` (-1 for none), and those every row carries the same way, on
/// the row `d` for the column at `ordinal`. `udt_schema` is the type's
/// schema; `db` the catalog.
pub(crate) fn fill(
    d: &mut Document,
    def: &TableDef,
    pg_type: &str,
    typmod: i32,
    ordinal: usize,
    udt_schema: &str,
    db: &str,
) {
    let mut put = |k: &str, v: Bson| {
        if let Some(f) = def.field_of(k) {
            d.insert(f, v);
        }
    };
    let none = typmod < 0;
    let char_max = match pg_type {
        "bpchar" | "varchar" if !none => int(typmod - 4),
        "bit" | "varbit" if !none => int(typmod),
        _ => Bson::Null,
    };
    let octets = match pg_type {
        "text" | "bpchar" | "varchar" if none => int(1 << 30),
        "bpchar" | "varchar" => int((typmod - 4) * MAX_ENCODING_LENGTH),
        _ => Bson::Null,
    };
    let (precision, radix, scale) = match pg_type {
        "int2" => (int(16), int(2), int(0)),
        "int4" => (int(32), int(2), int(0)),
        "int8" => (int(64), int(2), int(0)),
        "float4" => (int(24), int(2), Bson::Null),
        "float8" => (int(53), int(2), Bson::Null),
        "numeric" if none => (Bson::Null, int(10), Bson::Null),
        "numeric" => {
            let packed = typmod - 4;
            (int((packed >> 16) & 0xffff), int(10), int(packed & 0xffff))
        }
        _ => (Bson::Null, Bson::Null, Bson::Null),
    };
    let datetime = match pg_type {
        "date" => int(0),
        "time" | "timetz" | "timestamp" | "timestamptz" => int(if none { 6 } else { typmod }),
        "interval" if none || typmod & 0xffff == 0xffff => int(6),
        "interval" => int(typmod & 0xffff),
        _ => Bson::Null,
    };
    // `_pg_interval_type`: the field list `format_type` prints, upper-cased.
    let interval_type = match pg_type {
        "interval" if !none => {
            let text = secantus_pgplan::scalar::format_type_text_public("interval", Some(typmod));
            let fields = text
                .trim_start_matches("interval")
                .trim_start_matches(|c: char| c == '(' || c == ')' || c.is_ascii_digit())
                .trim();
            let fields = fields.split('(').next().unwrap_or("").trim();
            if fields.is_empty() {
                Bson::Null
            } else {
                Bson::String(fields.to_ascii_uppercase())
            }
        }
        _ => Bson::Null,
    };
    put("character_maximum_length", char_max);
    put("character_octet_length", octets);
    put("numeric_precision", precision);
    put("numeric_precision_radix", radix);
    put("numeric_scale", scale);
    put("datetime_precision", datetime);
    put("interval_type", interval_type);
    put("interval_precision", Bson::Null);
    put("udt_catalog", Bson::String(db.to_string()));
    put("udt_schema", Bson::String(udt_schema.to_string()));
    put("dtd_identifier", Bson::String(ordinal.to_string()));
    put("is_self_referencing", Bson::String("NO".into()));
    put("identity_cycle", Bson::String("NO".into()));
    put("is_updatable", Bson::String("YES".into()));
}

/// An identity column's sequence options, from its sequence's document.
pub(crate) fn fill_identity(d: &mut Document, def: &TableDef, seq: &Document) {
    let text = |k: &str, fallback: i64| {
        let v = match seq.get(k) {
            Some(Bson::Int32(v)) => i64::from(*v),
            Some(Bson::Int64(v)) => *v,
            _ => fallback,
        };
        Bson::String(v.to_string())
    };
    for (column, key, fallback) in [
        ("identity_start", "start", 1),
        ("identity_increment", "increment", 1),
        ("identity_maximum", "max_value", i64::MAX),
        ("identity_minimum", "min_value", 1),
    ] {
        if let Some(f) = def.field_of(column) {
            d.insert(f, text(key, fallback));
        }
    }
    if let Some(f) = def.field_of("identity_cycle") {
        let cycle = seq.get_bool("cycle").unwrap_or(false);
        d.insert(f, if cycle { "YES" } else { "NO" });
    }
}
