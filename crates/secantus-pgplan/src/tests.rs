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
            let ConstCol::Value(Bson::String(v)) = &sc.columns[0].1 else {
                panic!("version() must be a text value");
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
                (
                    "one".to_string(),
                    ConstCol::Value(Bson::Int32(1)),
                    "int4".to_string()
                )
            );
            assert_eq!(
                sc.columns[1],
                (
                    "current_database".to_string(),
                    ConstCol::Value(Bson::String("postgres".into())),
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
            assert_eq!(sc.columns[0].1, ConstCol::Value(Bson::Int32(1)));
            assert_eq!(sc.columns[0].2, "int4");
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // The value is NULL, but the declared type is still int4.
    match plan_with_params("SELECT $1::int", &lookup, &[Bson::Null]).unwrap() {
        Statement::SelectConstant(sc) => {
            assert_eq!(sc.columns[0].1, ConstCol::Value(Bson::Null));
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
            Statement::SelectConstant(sc) => {
                assert_eq!(sc.columns[0].1, ConstCol::Value(want), "for {sql}")
            }
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
            assert_eq!(sc.columns[0].1, ConstCol::Value(Bson::Null));
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

#[test]
fn session_settings_are_planned() {
    match plan_ok("SHOW client_encoding") {
        Statement::Show(n) => assert_eq!(n, "client_encoding"),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SET my.x = '7'") {
        Statement::Set { name, value } => {
            assert_eq!(name, "my.x");
            assert_eq!(value, "7");
        }
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("RESET my.x") {
        Statement::Reset(n) => assert_eq!(n, "my.x"),
        other => panic!("wrong statement: {other:?}"),
    }
    // RESET ALL is an empty name rather than its own variant.
    match plan_ok("RESET ALL") {
        Statement::Reset(n) => assert!(n.is_empty()),
        other => panic!("wrong statement: {other:?}"),
    }
}

/// The GUC functions resolve at EXECUTION, not while planning: the settings
/// live on the connection and the planner is stateless.
#[test]
fn guc_functions_defer_to_the_connection() {
    match plan_ok("SELECT current_setting('x')") {
        Statement::SelectConstant(sc) => assert_eq!(
            sc.columns[0].1,
            ConstCol::CurrentSetting {
                name: "x".into(),
                missing_ok: false
            }
        ),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT current_setting('x', true)") {
        Statement::SelectConstant(sc) => assert_eq!(
            sc.columns[0].1,
            ConstCol::CurrentSetting {
                name: "x".into(),
                missing_ok: true
            }
        ),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT set_config('a', 'b', false)") {
        Statement::SelectConstant(sc) => assert_eq!(
            sc.columns[0].1,
            ConstCol::SetConfig {
                name: "a".into(),
                value: Bson::String("b".into()),
                is_local: false,
            }
        ),
        other => panic!("wrong statement: {other:?}"),
    }
}

/// `date` and `time` are stored as canonical TEXT -- the same representation
/// the Python server writes, because the two share one store.
#[test]
fn date_and_time_canonicalise() {
    let cases = [
        ("SELECT '2026-09-01'::date", "2026-09-01"),
        // PostgreSQL accepts several spellings and renders exactly one.
        ("SELECT '2026-9-1'::date", "2026-09-01"),
        ("SELECT '20260901'::date", "2026-09-01"),
        ("SELECT '12:34:56'::time", "12:34:56"),
        ("SELECT '12:34'::time", "12:34:00"),
        // A fraction keeps only the digits that matter.
        ("SELECT '12:34:56.5'::time", "12:34:56.5"),
        ("SELECT '12:34:56.000'::time", "12:34:56"),
    ];
    for (sql, want) in cases {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => assert_eq!(
                sc.columns[0].1,
                ConstCol::Value(Bson::String(want.into())),
                "for {sql}"
            ),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// Malformed and impossible are DIFFERENT SQLSTATEs, probed on PG 14.
#[test]
fn bad_dates_distinguish_22007_from_22008() {
    // Not a date at all.
    for sql in ["SELECT 'not-a-date'::date", "SELECT 'xx:yy'::time"] {
        let err = plan(sql, &lookup).expect_err(sql);
        assert_eq!(err.sqlstate(), "22007", "for {sql}");
    }
    // Well-formed, but naming a value that cannot exist.
    for sql in ["SELECT '2026-02-30'::date", "SELECT '25:00:00'::time"] {
        let err = plan(sql, &lookup).expect_err(sql);
        assert_eq!(err.sqlstate(), "22008", "for {sql}");
    }
    // NULL survives every cast, including these.
    match plan_ok("SELECT NULL::date") {
        Statement::SelectConstant(sc) => assert_eq!(sc.columns[0].1, ConstCol::Value(Bson::Null)),
        other => panic!("wrong statement: {other:?}"),
    }
}

/// `numeric` keeps its SCALE, which is part of the value rather than
/// formatting: PostgreSQL answers `'1.50'`, not `'1.5'`.
#[test]
fn numeric_preserves_scale() {
    for (sql, want) in [
        ("SELECT 1.5", "1.5"),
        ("SELECT 1.50", "1.50"),
        ("SELECT '0.1'::numeric", "0.1"),
        ("SELECT '-0.30'::numeric", "-0.30"),
        ("SELECT '2.5000000000000000'::numeric", "2.5000000000000000"),
    ] {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => {
                let ConstCol::Value(Bson::Decimal128(d)) = &sc.columns[0].1 else {
                    panic!("{sql} should be a Decimal128, got {:?}", sc.columns[0].1);
                };
                assert_eq!(d.to_string(), want, "for {sql}");
                // A decimal literal is `numeric` in PostgreSQL, not float8.
                assert_eq!(sc.columns[0].2, "numeric", "for {sql}");
            }
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// Beyond 34 significant digits we REFUSE rather than round.
///
/// PostgreSQL's `numeric` is arbitrary precision and Decimal128 is not, so
/// there are values PostgreSQL accepts that this server cannot store. Quietly
/// rounding one would be a wrong answer; an error is a missing feature.
#[test]
fn numeric_refuses_rather_than_rounds() {
    let err = plan(
        "SELECT '1.2345678901234567890123456789012345'::numeric",
        &lookup,
    )
    .expect_err("35 significant digits");
    assert_eq!(err.sqlstate(), "22003"); // numeric_value_out_of_range
                                         // Not a number at all is a different code.
    let err = plan("SELECT 'x'::numeric", &lookup).expect_err("not numeric");
    assert_eq!(err.sqlstate(), "22P02");
}

/// Array text form: `{...}`, nested, with the quoting PostgreSQL uses.
///
/// An element is quoted only when leaving it bare would change how the array
/// reads back — a comma, a brace, a space, a quote, a backslash, or the bare
/// word NULL (which would otherwise become a real NULL).
#[test]
fn array_renders_like_postgres() {
    for (sql, want) in [
        ("SELECT ARRAY[1,2,3]::int[]", "{1,2,3}"),
        ("SELECT '{}'::int[]", "{}"),
        ("SELECT '{{1,2},{3,4}}'::int[]", "{{1,2},{3,4}}"),
        ("SELECT ARRAY['a','b']::text[]", "{a,b}"),
        ("SELECT ARRAY['a b']::text[]", "{\"a b\"}"),
        ("SELECT ARRAY['a,b']::text[]", "{\"a,b\"}"),
        ("SELECT ARRAY['NULL']::text[]", "{\"NULL\"}"),
        ("SELECT '{a,NULL,b}'::text[]", "{a,NULL,b}"),
    ] {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => {
                let ConstCol::Value(v) = &sc.columns[0].1 else {
                    panic!("{sql} should be a value, got {:?}", sc.columns[0].1);
                };
                let Bson::Array(items) = v else {
                    panic!("{sql} should be an Array, got {v:?}");
                };
                assert_eq!(render_array(items), want, "for {sql}");
            }
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// Inside an array, NULL does NOT behave the way it does in scalar SQL.
///
/// Scalar `NULL = NULL` is NULL, so the obvious implementation — compare
/// elementwise through the scalar path — gets every one of these wrong. All
/// four rules were probed against a live PostgreSQL 14 rather than reasoned
/// out; see the matching cases in `tests/test_rust_pgserver_differential.py`.
#[test]
fn array_comparison_follows_postgres_null_rules() {
    for (sql, want) in [
        // Two NULLs are EQUAL inside an array.
        ("SELECT ARRAY[NULL]::text[] = ARRAY[NULL]::text[]", true),
        // A NULL sorts AFTER any non-NULL element.
        (
            "SELECT ARRAY['a',NULL]::text[] > ARRAY['a','z']::text[]",
            true,
        ),
        // A common prefix makes the SHORTER array the smaller one.
        ("SELECT ARRAY['a']::text[] < ARRAY['a','b']::text[]", true),
        ("SELECT '{}'::int[] < ARRAY[1]::int[]", true),
        // Ordinary elementwise comparison, for contrast.
        ("SELECT ARRAY[1,2]::int[] = ARRAY[1,2]::int[]", true),
        ("SELECT ARRAY[1,2]::int[] = ARRAY[1,3]::int[]", false),
        ("SELECT ARRAY[1,2]::int[] < ARRAY[1,3]::int[]", true),
        // The first differing element decides, not the length.
        ("SELECT ARRAY[2]::int[] > ARRAY[1,9,9]::int[]", true),
        ("SELECT ARRAY[1,2,3]::int[] <> ARRAY[1,2]::int[]", true),
    ] {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => {
                assert_eq!(
                    sc.columns[0].1,
                    ConstCol::Value(Bson::Boolean(want)),
                    "for {sql}"
                );
            }
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// `int[]` is a DIFFERENT declared type from `int`, and losing the brackets is
/// silent: libpg_query keeps the array-ness in `array_bounds`, not in the type
/// name, so reading only the name types an array column as its element type.
///
/// That mistake reads as harmless — until a cast target loses its brackets
/// too, at which point `%s::text[] = %s::text[]` degrades to comparing two
/// rendered STRINGS. That happens to give the right answer often enough to
/// look fine, which is why this is pinned here.
#[test]
fn array_type_keeps_its_brackets() {
    match plan_ok("CREATE TABLE t (id int PRIMARY KEY, xs int[], names text[])") {
        Statement::CreateTable(ct) => {
            let types: Vec<&str> = ct.columns.iter().map(|c| c.pg_type.as_str()).collect();
            assert_eq!(types, vec!["int4", "int4[]", "text[]"]);
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

/// Splitting a multi-command string goes through the PARSER, not a scan for
/// `;`, so a semicolon inside a literal or a comment does not split the batch.
#[test]
fn split_statements_respects_quoting() {
    for (sql, want) in [
        ("select 1", vec!["select 1"]),
        ("select 1; select 2", vec!["select 1", " select 2"]),
        // A trailing or doubled semicolon produces no extra command.
        ("select 1;", vec!["select 1"]),
        ("select 1;;", vec!["select 1"]),
        (";", vec![]),
        ("", vec![]),
        // The semicolon here is DATA, not a separator.
        ("select 'a;b'", vec!["select 'a;b'"]),
        ("select 'a;b'; select 2", vec!["select 'a;b'", " select 2"]),
    ] {
        let got = split_statements(sql).expect("split");
        let want: Vec<String> = want.into_iter().map(|s| s.trim().to_string()).collect();
        assert_eq!(got, want, "for {sql:?}");
    }
}

/// The extended protocol takes ONE command: it has a single parameter list and
/// a single row description, which two commands cannot share. PostgreSQL says
/// so with 42601, not with "not supported".
#[test]
fn a_prepared_statement_refuses_several_commands() {
    let err = plan("select 1; select 2", &lookup).expect_err("two commands");
    assert_eq!(err.sqlstate(), "42601");
    assert_eq!(
        err.to_string(),
        "cannot insert multiple commands into a prepared statement"
    );
}

/// Casting to an integer uses TWO DIFFERENT rounding rules in PostgreSQL, and
/// using one for both is a wrong answer rather than a rounding preference.
///
/// numeric -> integer rounds half AWAY FROM ZERO; float -> integer rounds half
/// TO EVEN. Rust's `f64::round()` is the former, so it answered 3 for
/// `2.5::float8::int` where PostgreSQL answers 2. Measured on PostgreSQL 14.
#[test]
fn integer_casts_round_by_source_type() {
    for (sql, want) in [
        // numeric: half away from zero.
        ("SELECT 0.5::int", 1),
        ("SELECT 1.5::int", 2),
        ("SELECT 2.5::int", 3),
        ("SELECT 3.5::int", 4),
        ("SELECT -1.5::int", -2),
        ("SELECT -0.5::int", -1),
        ("SELECT 1.4::int", 1),
        // float8: half to even.
        ("SELECT 0.5::float8::int", 0),
        ("SELECT 1.5::float8::int", 2),
        ("SELECT 2.5::float8::int", 2),
        ("SELECT 3.5::float8::int", 4),
        ("SELECT -2.5::float8::int", -2),
    ] {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => {
                assert_eq!(
                    sc.columns[0].1,
                    ConstCol::Value(Bson::Int32(want)),
                    "for {sql}"
                );
            }
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// A `numeric` is rounded on its DIGITS, not through an f64.
///
/// Decimal128 carries up to 34 significant digits and an f64 has 15, so a big
/// value routed through a float would round twice and could land on a
/// different integer than PostgreSQL reports.
#[test]
fn numeric_to_integer_does_not_go_through_a_float() {
    // Exactly representable as i64, but NOT as f64.
    match plan_ok("SELECT '9007199254740993'::numeric::int8") {
        Statement::SelectConstant(sc) => {
            assert_eq!(
                sc.columns[0].1,
                ConstCol::Value(Bson::Int64(9_007_199_254_740_993))
            );
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // Too large for the target: an error, never a truncation.
    let err =
        plan("SELECT '12345678901234567890.5'::numeric::int8", &lookup).expect_err("out of range");
    assert_eq!(err.sqlstate(), "22003");
}

/// `pg_typeof` answers the DISPLAY name of the STATIC type.
#[test]
fn pg_typeof_reports_display_names() {
    for (sql, want) in [
        ("SELECT pg_typeof(1)", "integer"),
        ("SELECT pg_typeof(1::int8)", "bigint"),
        ("SELECT pg_typeof(1.5)", "numeric"),
        ("SELECT pg_typeof(1.5::float8)", "double precision"),
        ("SELECT pg_typeof('a'::varchar)", "character varying"),
        ("SELECT pg_typeof('a'::bpchar)", "character"),
        ("SELECT pg_typeof('12:00'::time)", "time without time zone"),
        ("SELECT pg_typeof(ARRAY[1,2])", "integer[]"),
        ("SELECT pg_typeof(ARRAY['a']::text[])", "text[]"),
        // Static, not read off the value: no value can report `unknown`.
        ("SELECT pg_typeof(null)", "unknown"),
        ("SELECT pg_typeof(1=1)", "boolean"),
    ] {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => {
                assert_eq!(
                    sc.columns[0].1,
                    ConstCol::Value(Bson::String(want.to_string())),
                    "for {sql}"
                );
                // A regtype, not text: a client reading oid 25 would print the
                // same characters but compare unequal to a regtype.
                assert_eq!(sc.columns[0].2, "regtype", "for {sql}");
            }
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// `SET TimeZone` uses the POSIX sign, which is the REVERSE of the sign in a
/// timestamp literal: `SET TimeZone TO '+02:00'` means two hours WEST of
/// Greenwich and renders as `-02`, while `'12:00+02'` is two hours EAST.
///
/// Probed against PostgreSQL 14. Getting this backwards is invisible in UTC
/// and wrong by four hours everywhere else.
#[test]
fn set_timezone_uses_the_posix_sign() {
    let east = |secs: i32| chrono::FixedOffset::east_opt(secs).unwrap();
    for (value, want) in [
        ("+02:00", east(-2 * 3600)),
        ("-02:00", east(2 * 3600)),
        ("+05:30", east(-(5 * 3600 + 30 * 60))),
        ("2", east(-2 * 3600)),
    ] {
        match TimeZoneSetting::parse(value) {
            TimeZoneSetting::Fixed(off) => assert_eq!(off, want, "for {value}"),
            other => panic!("{value} should be a fixed offset, got {other:?}"),
        }
    }
    assert_eq!(TimeZoneSetting::parse("UTC"), TimeZoneSetting::Utc);
    assert_eq!(TimeZoneSetting::parse("gmt"), TimeZoneSetting::Utc);
    assert!(matches!(
        TimeZoneSetting::parse("Europe/Rome"),
        TimeZoneSetting::Named(_)
    ));
    // An unknown name falls back to UTC rather than failing: the setting is
    // applied when it is SET, and refusing a later query would be worse.
    assert_eq!(TimeZoneSetting::parse("Mars/Olympus"), TimeZoneSetting::Utc);
}

/// A named zone carries a DST rule, so the SAME wall-clock reading resolves to
/// different offsets in January and July. A fixed offset does not.
#[test]
fn a_named_zone_changes_offset_across_dst() {
    let rome = TimeZoneSetting::parse("Europe/Rome");
    let jan = "2026-01-01 12:00";
    let jul = "2026-07-01 12:00";
    let of = |t: &str, tz: &TimeZoneSetting| {
        let micros = super::parse_timestamptz(t, tz).expect("parses");
        super::render_timestamptz(micros, tz)
    };
    assert_eq!(of(jan, &rome), "2026-01-01 12:00:00+01");
    assert_eq!(of(jul, &rome), "2026-07-01 12:00:00+02");

    let fixed = TimeZoneSetting::parse("-02:00"); // POSIX: two hours EAST
    assert_eq!(of(jan, &fixed), "2026-01-01 12:00:00+02");
    assert_eq!(of(jul, &fixed), "2026-07-01 12:00:00+02");
}

/// An offset can carry MINUTES and SECONDS, and both must survive the round
/// trip. A comment here once claimed no zone in use carried seconds; psycopg's
/// own corpus contains `+01:02:03`.
#[test]
fn offsets_keep_their_minutes_and_seconds() {
    let utc = TimeZoneSetting::Utc;
    for (literal, want) in [
        ("2000-01-01 00:00+01:02:03", "1999-12-31 22:57:57+00"),
        ("2026-01-01 12:00+02", "2026-01-01 10:00:00+00"),
        ("2026-01-01 12:00+05:30", "2026-01-01 06:30:00+00"),
        ("2026-01-01 12:00Z", "2026-01-01 12:00:00+00"),
        ("2026-01-01 12:00-02", "2026-01-01 14:00:00+00"),
    ] {
        let micros = super::parse_timestamptz(literal, &utc).expect("parses");
        assert_eq!(
            super::render_timestamptz(micros, &utc),
            want,
            "for {literal}"
        );
    }
}

/// A `-` inside a DATE must not be mistaken for the start of an offset.
#[test]
fn a_dates_hyphen_is_not_an_offset() {
    let utc = TimeZoneSetting::Utc;
    let micros = super::parse_timestamptz("2026-01-01 12:00", &utc).expect("parses");
    assert_eq!(
        super::render_timestamptz(micros, &utc),
        "2026-01-01 12:00:00+00"
    );
    // A bare date has no time at all, so nothing that follows can be an offset.
    let micros = super::parse_timestamptz("2026-01-01", &utc).expect("parses");
    assert_eq!(
        super::render_timestamptz(micros, &utc),
        "2026-01-01 00:00:00+00"
    );
}
