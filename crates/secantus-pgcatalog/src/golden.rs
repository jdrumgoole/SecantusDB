//! Golden vectors for the on-disk catalog format.
//!
//! Captured from the PYTHON server on 2026-08-31 by creating
//! `CREATE TABLE t (id int PRIMARY KEY, name text, n int)` against a real
//! `Storage` and dumping `__sql_catalog__`. The three servers share one store,
//! so a drift here is not a formatting nit -- it is one server writing a
//! catalog another reads as truth. Regenerate only against a fresh capture, and
//! never to make a failing test pass.

use super::*;

fn t_table() -> TableDef {
    TableDef::new(
        "t",
        vec![
            Column::new("id", "int4", true),
            Column::new("name", "text", false),
            Column::new("n", "int4", false),
        ],
    )
}

#[test]
fn table_document_matches_the_python_server() {
    let got = t_table().to_document();

    // Verbatim from the capture.
    let want = doc! {
        "_id": "t",
        "table": "t",
        "collection": "t",
        "columns": [
            doc! {
                "name": "id", "type": "int4", "field": "_id", "pk": true,
                "nullable": false, "has_default": false,
                "default": Bson::Null, "default_expr": Bson::Null,
                "comment": Bson::Null, "sequence": Bson::Null,
                "identity": Bson::Null, "enum_type": Bson::Null,
                "domain_type": Bson::Null, "generated": Bson::Null,
                "composite_type": Bson::Null, "composite_fields": Bson::Null,
                "json_plain": false, "decl_oid": Bson::Null, "typmod": -1i32,
            },
            doc! {
                "name": "name", "type": "text", "field": "name", "pk": false,
                "nullable": true, "has_default": false,
                "default": Bson::Null, "default_expr": Bson::Null,
                "comment": Bson::Null, "sequence": Bson::Null,
                "identity": Bson::Null, "enum_type": Bson::Null,
                "domain_type": Bson::Null, "generated": Bson::Null,
                "composite_type": Bson::Null, "composite_fields": Bson::Null,
                "json_plain": false, "decl_oid": Bson::Null, "typmod": -1i32,
            },
            doc! {
                "name": "n", "type": "int4", "field": "n", "pk": false,
                "nullable": true, "has_default": false,
                "default": Bson::Null, "default_expr": Bson::Null,
                "comment": Bson::Null, "sequence": Bson::Null,
                "identity": Bson::Null, "enum_type": Bson::Null,
                "domain_type": Bson::Null, "generated": Bson::Null,
                "composite_type": Bson::Null, "composite_fields": Bson::Null,
                "json_plain": false, "decl_oid": Bson::Null, "typmod": -1i32,
            },
        ],
        "comment": Bson::Null,
        "pk_name": Bson::Null,
        "pk_comment": Bson::Null,
        "pk_column_order": Bson::Null,
        "temp": false,
        "foreign_keys": [],
        "check_constraints": [],
        "unique_constraints": [],
        "expr_indexes": [],
    };

    assert_eq!(got, want);
}

/// Key ORDER is part of the contract, not just key presence: BSON preserves it,
/// and a document comparison alone would not catch a reordering.
#[test]
fn key_order_matches_the_python_server() {
    let got = t_table().to_document();
    let keys: Vec<&str> = got.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "_id",
            "table",
            "collection",
            "columns",
            "comment",
            "pk_name",
            "pk_comment",
            "pk_column_order",
            "temp",
            "foreign_keys",
            "check_constraints",
            "unique_constraints",
            "expr_indexes",
        ]
    );

    let col = got.get_array("columns").unwrap()[0].as_document().unwrap();
    let col_keys: Vec<&str> = col.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        col_keys,
        vec![
            "name",
            "type",
            "field",
            "pk",
            "nullable",
            "has_default",
            "default",
            "default_expr",
            "comment",
            "sequence",
            "identity",
            "enum_type",
            "domain_type",
            "generated",
            "composite_type",
            "composite_fields",
            "json_plain",
            "decl_oid",
            "typmod",
        ]
    );
}

#[test]
fn the_primary_key_column_is_stored_as_id() {
    let t = t_table();
    assert_eq!(t.field_of("id").as_deref(), Some("_id"));
    assert_eq!(t.field_of("name").as_deref(), Some("name"));
    // An unknown column must not silently become a field -- the caller owes
    // the client PostgreSQL's 42703.
    assert_eq!(t.field_of("nope"), None);
}

#[test]
fn round_trips_through_its_own_document() {
    let t = t_table();
    assert_eq!(TableDef::from_document(&t.to_document()), Some(t));
}
