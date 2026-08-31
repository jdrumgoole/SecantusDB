//! Lowering tests. The answers these pin were checked against a live
//! PostgreSQL 14 on 2026-08-31 -- PG is the oracle, not the Python server.

use super::*;
use bson::Bson;

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
        // `<>` carries an explicit not-null guard: MQL's `$ne` matches a
        // missing-or-null field, but SQL's `<>` over NULL yields NULL and the
        // row is excluded (probed PG 14).
        (
            "SELECT * FROM t WHERE name <> 'bob'",
            doc! {"$and": [
                {"name": {"$ne": "bob"}},
                {"name": {"$ne": Bson::Null}},
            ]},
        ),
        (
            "SELECT * FROM t WHERE n >= 20 AND id <> 3",
            doc! {"$and": [
                {"n": {"$gte": 20i32}},
                {"$and": [{"_id": {"$ne": 3i32}}, {"_id": {"$ne": Bson::Null}}]},
            ]},
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
        ("SELECT * FROM t JOIN u ON t.id = u.id", "0A000"),
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
/// NOT is pushed into the leaves rather than wrapped.
///
/// MQL has no operator matching SQL's NOT: `$nor` matches a missing-or-null
/// field where SQL yields NULL and excludes the row. De Morgan is valid in
/// three-valued logic, so the negation can descend to leaves that are already
/// NULL-correct. Every expectation below was checked against live PostgreSQL 14.
#[test]
fn not_is_pushed_down_to_null_correct_leaves() {
    let cases: Vec<(&str, Document)> = vec![
        (
            "SELECT * FROM t WHERE NOT (n IS NULL)",
            doc! {"n": {"$ne": Bson::Null}},
        ),
        (
            "SELECT * FROM t WHERE NOT (n IS NOT NULL)",
            doc! {"n": Bson::Null},
        ),
        (
            "SELECT * FROM t WHERE NOT (n > 1)",
            doc! {"n": {"$lte": 1i32}},
        ),
        // NOT NOT collapses rather than nesting.
        ("SELECT * FROM t WHERE NOT (NOT (n = 1))", doc! {"n": 1i32}),
        // De Morgan: NOT (a AND b) -> NOT a OR NOT b.
        (
            "SELECT * FROM t WHERE NOT (n = 1 AND name = 'bob')",
            doc! {"$or": [
                {"$and": [{"n": {"$ne": 1i32}}, {"n": {"$ne": Bson::Null}}]},
                {"$and": [{"name": {"$ne": "bob"}}, {"name": {"$ne": Bson::Null}}]},
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
fn order_limit_and_offset_are_planned() {
    match plan_ok("SELECT id FROM t ORDER BY n DESC, name LIMIT 5 OFFSET 2") {
        Statement::Select(s) => {
            assert_eq!(s.limit, Some(5));
            assert_eq!(s.offset, 2);
            assert_eq!(s.order.len(), 2);
            assert_eq!(s.order[0].field, "n");
            assert!(!s.order[0].ascending);
            // PostgreSQL's DESC default is NULLS FIRST; ASC is NULLS LAST.
            assert_eq!(s.order[0].nulls, Nulls::First);
            assert_eq!(s.order[1].field, "name");
            assert!(s.order[1].ascending);
            assert_eq!(s.order[1].nulls, Nulls::Last);
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn explicit_nulls_placement_overrides_the_direction_default() {
    match plan_ok("SELECT id FROM t ORDER BY n ASC NULLS FIRST") {
        Statement::Select(s) => assert_eq!(s.order[0].nulls, Nulls::First),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT id FROM t ORDER BY n DESC NULLS LAST") {
        Statement::Select(s) => assert_eq!(s.order[0].nulls, Nulls::Last),
        other => panic!("wrong statement: {other:?}"),
    }
}

/// `LIMIT NULL` means "no limit" in PostgreSQL, NOT "limit zero" -- while
/// `LIMIT 0` is a real limit that returns nothing.
#[test]
fn limit_null_is_not_limit_zero() {
    match plan_ok("SELECT id FROM t LIMIT NULL") {
        Statement::Select(s) => assert_eq!(s.limit, None),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT id FROM t LIMIT 0") {
        Statement::Select(s) => assert_eq!(s.limit, Some(0)),
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn in_and_between_respect_three_valued_logic() {
    let cases: Vec<(&str, Document)> = vec![
        (
            "SELECT * FROM t WHERE n IN (1, 3)",
            doc! {"n": {"$in": [1i32, 3i32]}},
        ),
        // A NULL in a positive IN list simply never matches, so it is dropped.
        (
            "SELECT * FROM t WHERE n IN (1, NULL)",
            doc! {"n": {"$in": [1i32]}},
        ),
        // NOT IN must exclude NULL rows: `NULL <> 1` is NULL, not true.
        (
            "SELECT * FROM t WHERE n NOT IN (1)",
            doc! {"$and": [{"n": {"$nin": [1i32]}}, {"n": {"$ne": Bson::Null}}]},
        ),
        (
            "SELECT * FROM t WHERE n BETWEEN 1 AND 3",
            doc! {"n": {"$gte": 1i32, "$lte": 3i32}},
        ),
    ];
    for (sql, want) in cases {
        match plan_ok(sql) {
            Statement::Select(s) => assert_eq!(s.filter, want, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
    // `NOT IN` over a list containing NULL is never true for any row.
    match plan_ok("SELECT * FROM t WHERE n NOT IN (1, NULL)") {
        Statement::Select(s) => assert_eq!(s.filter, doc! {"$nor": [Document::new()]}),
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn update_and_delete_are_planned() {
    match plan_ok("UPDATE t SET n = 5 WHERE id = 1") {
        Statement::Update(u) => {
            assert_eq!(u.table, "t");
            assert_eq!(u.set, doc! {"n": 5i32});
            assert_eq!(u.filter, doc! {"_id": 1i32});
        }
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("DELETE FROM t WHERE n > 2") {
        Statement::Delete(d) => {
            assert_eq!(d.table, "t");
            assert_eq!(d.filter, doc! {"n": {"$gt": 2i32}});
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // The PK is the document's `_id`, which storage treats as immutable.
    let err = plan("UPDATE t SET id = 2 WHERE id = 1", &lookup).expect_err("PK update");
    assert_eq!(err.sqlstate(), "0A000");
}

#[test]
fn shapes_sqlglot_mis_parses_reach_us_as_real_statements() {
    // `DROP TABLE a, b, c` and `BEGIN ...` used to live here too; both now
    // EXECUTE rather than merely parsing, which is the stronger result.
    for sql in [
        "MOVE FORWARD 2 IN c",
        "LISTEN chan",
        "NOTIFY chan, 'payload'",
        "COPY t FROM stdin WITH (freeze on)",
    ] {
        let err = plan(sql, &lookup).expect_err(sql);
        assert_eq!(err.sqlstate(), "0A000", "for {sql} (got {err})");
    }
}

/// Aggregates plan to POSITIONAL output columns.
///
/// `SELECT count(*), count(n)` yields two columns both called `count`. An
/// earlier cut keyed the result row by name, so the second silently overwrote
/// the first and `count(*)` reported `count(n)`'s answer.
#[test]
fn duplicate_aggregate_names_stay_distinct_columns() {
    match plan_ok("SELECT count(*), count(n) FROM t") {
        Statement::Aggregate(a) => {
            assert_eq!(a.items.len(), 2);
            assert_eq!(a.items[0].func, AggFunc::CountStar);
            assert_eq!(a.items[0].field, None);
            assert_eq!(a.items[1].func, AggFunc::Count);
            assert_eq!(a.items[1].field.as_deref(), Some("n"));
            assert_eq!(
                a.select,
                vec![
                    ("count".to_string(), OutputCol::Agg(0)),
                    ("count".to_string(), OutputCol::Agg(1)),
                ]
            );
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

/// ORDER BY over a column that is neither grouped nor aggregated is refused
/// rather than silently ignored.
#[test]
fn order_by_a_non_grouped_column_is_refused() {
    let err = plan("SELECT count(*) FROM t ORDER BY name", &lookup).expect_err("must refuse");
    assert_eq!(err.sqlstate(), "0A000");
}

#[test]
fn group_by_resolves_order_by_index() {
    match plan_ok("SELECT count(*) FROM t GROUP BY name ORDER BY name DESC") {
        Statement::Aggregate(a) => {
            assert_eq!(a.group_by, vec![("name".to_string(), "name".to_string())]);
            // `name` is grouped but NOT projected; only the aggregate is.
            assert_eq!(a.select, vec![("count".to_string(), OutputCol::Agg(0))]);
            assert_eq!(a.order.len(), 1);
            assert_eq!(a.order[0].group_index, 0);
            assert!(!a.order[0].ascending);
            // DESC defaults to NULLS FIRST.
            assert_eq!(a.order[0].nulls, Nulls::First);
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn min_and_max_carry_their_source_type() {
    match plan_ok("SELECT min(n), max(name) FROM t") {
        Statement::Aggregate(a) => {
            assert_eq!(a.items[0].source_type.as_deref(), Some("int4"));
            assert_eq!(a.items[1].source_type.as_deref(), Some("text"));
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn aggregate_refusals_carry_the_right_sqlstate() {
    let cases: Vec<(&str, &str)> = vec![
        // A bare column beside an aggregate must be grouped.
        ("SELECT name, count(*) FROM t", "42803"),
        // Deliberately deferred rather than approximated.
        ("SELECT avg(n) FROM t", "0A000"),
        ("SELECT count(DISTINCT n) FROM t", "0A000"),
        ("SELECT count(*) FROM t HAVING count(*) > 1", "0A000"),
        ("SELECT sum(n + 1) FROM t", "0A000"),
        ("SELECT count(nope) FROM t", "42703"),
        ("SELECT count(*) FROM t GROUP BY nope", "42703"),
    ];
    for (sql, want) in cases {
        let err = plan(sql, &lookup).expect_err(sql);
        assert_eq!(err.sqlstate(), want, "for {sql} (got {err})");
    }
}

/// `$N` placeholders resolve from the extended protocol's bound values.
#[test]
fn bound_parameters_substitute_into_the_plan() {
    let params = vec![Bson::Int32(5), Bson::String("bob".into())];
    match plan_with_params("SELECT id FROM t WHERE n > $1", &lookup, &params).unwrap() {
        Statement::Select(s) => assert_eq!(s.filter, doc! {"n": {"$gt": 5i32}}),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_with_params("SELECT id FROM t WHERE name = $2", &lookup, &params).unwrap() {
        Statement::Select(s) => assert_eq!(s.filter, doc! {"name": "bob"}),
        other => panic!("wrong statement: {other:?}"),
    }
    // A bound value works anywhere a literal does.
    match plan_with_params("SELECT id FROM t LIMIT $1", &lookup, &params).unwrap() {
        Statement::Select(s) => assert_eq!(s.limit, Some(5)),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_with_params("UPDATE t SET n = $1 WHERE name = $2", &lookup, &params).unwrap() {
        Statement::Update(u) => {
            assert_eq!(u.set, doc! {"n": 5i32});
            assert_eq!(u.filter, doc! {"name": "bob"});
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

/// A comparison against NULL is never TRUE -- bound or literal.
///
/// `n = NULL` yields NULL, not true, so no row qualifies; only `IS NULL`
/// matches. MQL's `{n: null}` WOULD match, so the lowering short-circuits.
/// Probed PG 14. Found by the parameterised differential, but the literal form
/// was equally wrong and equally untested.
#[test]
fn comparing_against_null_matches_nothing() {
    let never = doc! {"$nor": [Document::new()]};
    for sql in [
        "SELECT id FROM t WHERE n = NULL",
        "SELECT id FROM t WHERE n <> NULL",
        "SELECT id FROM t WHERE n > NULL",
        "SELECT id FROM t WHERE n <= NULL",
    ] {
        match plan_ok(sql) {
            Statement::Select(s) => assert_eq!(s.filter, never, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
    // The same, arriving as a bound parameter.
    let params = vec![Bson::Null];
    match plan_with_params("SELECT id FROM t WHERE n = $1", &lookup, &params).unwrap() {
        Statement::Select(s) => assert_eq!(s.filter, never),
        other => panic!("wrong statement: {other:?}"),
    }
}

/// A `$N` with nothing bound is a client error, not a panic.
#[test]
fn an_unbound_parameter_is_42p02() {
    let err = plan_with_params("SELECT id FROM t WHERE n = $2", &lookup, &[Bson::Int32(1)])
        .expect_err("must refuse");
    assert_eq!(err.sqlstate(), "42P02");
}

/// Transaction control is planned, not refused.
///
/// psycopg wraps even its connection setup in BEGIN/COMMIT, so a server that
/// refuses these is unusable by real clients no matter how good its queries
/// are -- the psycopg gauge failed on the very first statement until this
/// existed.
#[test]
fn transaction_statements_are_planned() {
    for (sql, want) in [
        ("BEGIN", TransactionControl::Begin),
        ("START TRANSACTION", TransactionControl::Begin),
        (
            "BEGIN ISOLATION LEVEL SERIALIZABLE READ WRITE",
            TransactionControl::Begin,
        ),
        ("COMMIT", TransactionControl::Commit),
        ("ROLLBACK", TransactionControl::Rollback),
    ] {
        match plan_ok(sql) {
            Statement::Transaction(c) => assert_eq!(c, want, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
    // Savepoints would need machinery this server does not have; refusing is
    // honest, silently accepting would lose the semantics a client relies on.
    let err = plan("SAVEPOINT s1", &lookup).expect_err("savepoint");
    assert_eq!(err.sqlstate(), "0A000");
}

/// `SELECT` with no FROM, including the session functions a connecting client
/// probes before it does anything else.
#[test]
fn select_without_from_answers_session_functions() {
    match plan_ok("SELECT version()") {
        Statement::SelectConstant(sc) => {
            assert_eq!(sc.columns.len(), 1);
            assert_eq!(sc.columns[0].0, "version");
            let Bson::String(v) = &sc.columns[0].1 else {
                panic!("version() must be text");
            };
            // The gauges refuse to score a daemon whose version() does not
            // name SecantusDB, so a real PostgreSQL cannot inflate the number.
            assert!(v.contains("SecantusDB"), "{v}");
        }
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT 1 AS one, current_database()") {
        Statement::SelectConstant(sc) => {
            assert_eq!(
                sc.columns[0],
                ("one".to_string(), Bson::Int32(1), "int4".to_string())
            );
            assert_eq!(
                sc.columns[1],
                (
                    "current_database".to_string(),
                    Bson::String("postgres".into()),
                    "text".to_string()
                )
            );
        }
        other => panic!("wrong statement: {other:?}"),
    }
    let err = plan("SELECT nosuchfunc()", &lookup).expect_err("unknown function");
    assert_eq!(err.sqlstate(), "0A000");
}

/// A cast DECLARES its column's type, which is not the same as the type of the
/// value that turns up.
///
/// `Describe` runs before `Bind`, so it plans against NULL placeholders. Typing
/// the column from the value made `$1::int` a `varchar`, and the client then
/// decoded a correct integer as a string.
#[test]
fn a_cast_declares_the_column_type() {
    match plan_ok("SELECT '1'::int") {
        Statement::SelectConstant(sc) => {
            assert_eq!(sc.columns[0].1, Bson::Int32(1));
            assert_eq!(sc.columns[0].2, "int4");
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // The value is NULL, but the declared type is still int4.
    match plan_with_params("SELECT $1::int", &lookup, &[Bson::Null]).unwrap() {
        Statement::SelectConstant(sc) => {
            assert_eq!(sc.columns[0].1, Bson::Null);
            assert_eq!(sc.columns[0].2, "int4");
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // A value that cannot convert is 22P02, quoting the input as PostgreSQL does.
    let err = plan("SELECT 'x'::int", &lookup).expect_err("bad cast");
    assert_eq!(err.sqlstate(), "22P02");
    assert!(
        err.to_string()
            .contains("invalid input syntax for type integer"),
        "{err}"
    );
}

#[test]
fn drop_table_is_planned() {
    match plan_ok("DROP TABLE t") {
        Statement::DropTable(d) => {
            assert_eq!(d.tables, vec!["t".to_string()]);
            assert!(!d.if_exists);
        }
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("DROP TABLE IF EXISTS a, b") {
        Statement::DropTable(d) => {
            assert_eq!(d.tables, vec!["a".to_string(), "b".to_string()]);
            assert!(d.if_exists);
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // CASCADE would have to chase dependants; behaving as RESTRICT silently
    // would be the wrong kind of helpful.
    let err = plan("DROP TABLE t CASCADE", &lookup).expect_err("cascade");
    assert_eq!(err.sqlstate(), "0A000");
    // Other DROP targets stay refused rather than dropping the wrong thing.
    let err = plan("DROP INDEX i", &lookup).expect_err("drop index");
    assert_eq!(err.sqlstate(), "0A000");
}

/// Constant expressions, with the corners PostgreSQL gets surprising.
#[test]
fn constant_expressions_follow_postgres() {
    let cases: Vec<(&str, Bson)> = vec![
        ("SELECT 1+1", Bson::Int32(2)),
        ("SELECT 1-2", Bson::Int32(-1)),
        ("SELECT 2*3", Bson::Int32(6)),
        // Integer division TRUNCATES: 7/2 is 3, not 3.5.
        ("SELECT 7/2", Bson::Int32(3)),
        ("SELECT 7%2", Bson::Int32(1)),
        ("SELECT (1+2)*3", Bson::Int32(9)),
        ("SELECT -3", Bson::Int32(-3)),
        ("SELECT 'a'||'b'", Bson::String("ab".into())),
        // `||` coerces the non-text side.
        ("SELECT 'n='||1", Bson::String("n=1".into())),
        // NULL propagates through every operator.
        ("SELECT 1+NULL", Bson::Null),
        ("SELECT 1=1", Bson::Boolean(true)),
        ("SELECT 2<>2", Bson::Boolean(false)),
    ];
    for (sql, want) in cases {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => assert_eq!(sc.columns[0].1, want, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
    let err = plan("SELECT 5/0", &lookup).expect_err("division by zero");
    assert_eq!(err.sqlstate(), "22012");
}

/// An expression's column type comes from the OPERATOR, not from the value.
///
/// `Describe` precedes `Bind`, so `SELECT $1 + 1` evaluates to NULL when the
/// type is decided. Reading the type off that NULL would call it `text`.
#[test]
fn an_expression_is_typed_by_its_operator() {
    match plan_with_params("SELECT $1 + 1", &lookup, &[Bson::Null]).unwrap() {
        Statement::SelectConstant(sc) => {
            assert_eq!(sc.columns[0].1, Bson::Null);
            assert_eq!(sc.columns[0].2, "int4");
        }
        other => panic!("wrong statement: {other:?}"),
    }
    for (sql, want) in [("SELECT 'a'||'b'", "text"), ("SELECT 1<2", "bool")] {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => assert_eq!(sc.columns[0].2, want, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}
