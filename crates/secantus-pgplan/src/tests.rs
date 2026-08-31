//! Lowering tests. The answers these pin were checked against a live
//! PostgreSQL 14 on 2026-08-31 -- PG is the oracle, not the Python server.

use super::*;

fn t() -> TableDef {
    TableDef::new(
        "t",
        vec![
            Column::new("id", "int4", true),
            Column::new("name", "text", false),
            Column::new("n", "int4", false),
        ],
    )
}

fn lookup(name: &str) -> Option<TableDef> {
    (name == "t").then(t)
}

fn plan_ok(sql: &str) -> Statement {
    plan(sql, &lookup).expect("should plan")
}

#[test]
fn create_table_maps_the_primary_key_onto_id() {
    match plan_ok("CREATE TABLE t (id int PRIMARY KEY, name text, n int)") {
        Statement::CreateTable(def) => {
            assert_eq!(def.name, "t");
            assert_eq!(def.columns.len(), 3);
            // libpg_query qualifies built-ins; the catalog stores the bare name.
            assert_eq!(def.columns[0].pg_type, "int4");
            assert_eq!(def.columns[1].pg_type, "text");
            assert!(def.columns[0].pk);
            assert!(!def.columns[0].nullable);
            assert_eq!(def.columns[0].field(), "_id");
            assert_eq!(def.columns[1].field(), "name");
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn insert_keys_rows_by_stored_field() {
    match plan_ok("INSERT INTO t VALUES (1, 'alice', 10), (2, 'bob', 20)") {
        Statement::Insert(i) => {
            assert_eq!(i.table, "t");
            assert_eq!(i.rows.len(), 2);
            // The PK arrives as `_id`, which is what makes SQL PK uniqueness
            // ride the storage layer's own index.
            assert_eq!(i.rows[0], doc! {"_id": 1i32, "name": "alice", "n": 10i32});
            assert_eq!(i.rows[1], doc! {"_id": 2i32, "name": "bob", "n": 20i32});
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn insert_honours_an_explicit_column_list_and_its_order() {
    match plan_ok("INSERT INTO t (n, id) VALUES (7, 3)") {
        Statement::Insert(i) => assert_eq!(i.rows[0], doc! {"n": 7i32, "_id": 3i32}),
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn select_star_expands_in_declared_order() {
    match plan_ok("SELECT * FROM t") {
        Statement::Select(s) => {
            assert_eq!(
                s.columns,
                vec![
                    ("id".into(), "_id".into()),
                    ("name".into(), "name".into()),
                    ("n".into(), "n".into()),
                ]
            );
            assert_eq!(s.filter, Document::new());
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn select_lowers_predicates_over_stored_fields() {
    let cases: Vec<(&str, Document)> = vec![
        ("SELECT * FROM t WHERE id = 1", doc! {"_id": 1i32}),
        ("SELECT * FROM t WHERE n > 15", doc! {"n": {"$gt": 15i32}}),
        (
            "SELECT * FROM t WHERE name <> 'bob'",
            doc! {"name": {"$ne": "bob"}},
        ),
        (
            "SELECT * FROM t WHERE n >= 20 AND id <> 3",
            doc! {"$and": [{"n": {"$gte": 20i32}}, {"_id": {"$ne": 3i32}}]},
        ),
        (
            "SELECT * FROM t WHERE name = 'carol' OR n < 15",
            doc! {"$or": [{"name": "carol"}, {"n": {"$lt": 15i32}}]},
        ),
        (
            "SELECT * FROM t WHERE n <= 20 AND (id = 1 OR name = 'bob')",
            doc! {"$and": [
                {"n": {"$lte": 20i32}},
                {"$or": [{"_id": 1i32}, {"name": "bob"}]},
            ]},
        ),
    ];
    for (sql, want) in cases {
        match plan_ok(sql) {
            Statement::Select(s) => assert_eq!(s.filter, want, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

#[test]
fn select_renames_through_an_alias() {
    match plan_ok("SELECT name AS who FROM t") {
        Statement::Select(s) => assert_eq!(s.columns, vec![("who".into(), "name".into())]),
        other => panic!("wrong statement: {other:?}"),
    }
}

/// Every refusal is a specific SQLSTATE. Answering a generic error, or worse a
/// row, is the failure mode this server exists to avoid.
#[test]
fn unsupported_and_undefined_carry_postgres_sqlstates() {
    let cases: Vec<(&str, &str)> = vec![
        ("SELECT count(*) FROM t GROUP BY n", "0A000"),
        ("SELECT * FROM t JOIN u ON t.id = u.id", "0A000"),
        ("SELECT * FROM t WHERE NOT (n = 1)", "0A000"),
        ("SELECT * FROM t WHERE n LIKE 'x'", "0A000"),
        ("SELECT nope FROM t", "42703"),
        ("SELECT * FROM t WHERE nope = 1", "42703"),
        ("SELECT * FROM missing", "42P01"),
        ("INSERT INTO missing VALUES (1)", "42P01"),
        (
            "CREATE TABLE t (a int PRIMARY KEY, b int PRIMARY KEY)",
            "0A000",
        ),
        ("SELECT !!! FROM", "42601"),
    ];
    for (sql, want) in cases {
        let err = plan(sql, &lookup).expect_err(sql);
        assert_eq!(err.sqlstate(), want, "for {sql} (got {err})");
    }
}

/// The shapes the backlog records sqlglot mangling. Each needs a regex
/// pre-pass in the Python planner; libpg_query parses them natively, so they
/// must reach an honest 0A000 rather than a syntax error.
#[test]
fn shapes_sqlglot_mis_parses_reach_us_as_real_statements() {
    for sql in [
        "MOVE FORWARD 2 IN c",
        "LISTEN chan",
        "NOTIFY chan, 'payload'",
        "DROP TABLE a, b, c",
        "BEGIN ISOLATION LEVEL SERIALIZABLE READ WRITE",
        "COPY t FROM stdin WITH (freeze on)",
    ] {
        let err = plan(sql, &lookup).expect_err(sql);
        assert_eq!(err.sqlstate(), "0A000", "for {sql} (got {err})");
    }
}
