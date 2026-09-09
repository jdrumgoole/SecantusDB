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
        Statement::CreateTable(def, _) => {
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
        // A JOIN is planned now (enum/range info work); a missing side is
        // the same 42P01 PostgreSQL gives, not the old blanket 0A000.
        ("SELECT a FROM t JOIN u ON t.id = u.id", "42P01"),
        // A JOIN shape the planner does not reduce to two-table/one-ON is
        // still refused rather than half-run.
        ("SELECT a FROM t JOIN u ON t.id > u.id", "42P01"),
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
    // `DROP TABLE a, b, c`, `BEGIN ...`, `MOVE FORWARD 2 IN c` and
    // `NOTIFY chan, 'payload'` used to live here too; all four now EXECUTE
    // rather than merely parsing, which is the stronger result.
    for sql in ["LISTEN chan", "COPY t FROM stdin WITH (freeze on)"] {
        let err = plan(sql, &lookup).expect_err(sql);
        assert_eq!(err.sqlstate(), "0A000", "for {sql} (got {err})");
    }
    assert!(matches!(
        plan_ok("NOTIFY chan, 'payload'"),
        Statement::Notify
    ));
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
            assert_eq!(a.group_by.len(), 1);
            assert_eq!(a.group_by[0].name, "name");
            assert_eq!(a.group_by[0].field, "name");
            assert!(a.group_by[0].expr.is_none());
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

/// `GROUP BY <expression>` and `GROUP BY <position>` both key on the
/// expression, evaluated per row; a projected copy of the same expression is
/// the group column, matched by structure. Types and names probed on PG 16:
/// `length(data)` is `int4` named `length`, `x is null` is `bool` named
/// `?column?`.
#[test]
fn group_by_an_expression_or_a_position_keys_on_the_expression() {
    for sql in [
        "SELECT length(name), name IS NULL, count(*) FROM t GROUP BY length(name), name IS NULL",
        "SELECT length(name), name IS NULL, count(*) FROM t GROUP BY 1, 2",
        "SELECT length(name) AS length, name IS NULL, count(*) FROM t GROUP BY length, 2",
    ] {
        match plan(sql, &lookup).unwrap_or_else(|e| panic!("{sql}: {e:?}")) {
            Statement::Aggregate(a) => {
                assert_eq!(a.group_by.len(), 2, "{sql}");
                assert_eq!(a.group_by[0].name, "length");
                assert_eq!(a.group_by[0].pg_type, "int4");
                assert_eq!(a.group_by[1].name, "?column?");
                assert_eq!(a.group_by[1].pg_type, "bool");
                let row = doc! { "_id": 1, "name": "ab", "n": 3 };
                let key0 = a.group_by[0].expr.as_ref().expect("an expression key");
                assert_eq!(apply_row_expr(key0, &row).unwrap(), Bson::Int32(2));
                let key1 = a.group_by[1].expr.as_ref().expect("an expression key");
                assert_eq!(apply_row_expr(key1, &row).unwrap(), Bson::Boolean(false));
                assert_eq!(
                    a.select,
                    vec![
                        ("length".to_string(), OutputCol::Group(0)),
                        ("?column?".to_string(), OutputCol::Group(1)),
                        ("count".to_string(), OutputCol::Agg(0)),
                    ],
                    "{sql}"
                );
            }
            other => panic!("wrong statement: {other:?}"),
        }
    }
    // A position past the select list is 42P10, worded as PostgreSQL words it.
    let err =
        plan("SELECT length(name), count(*) FROM t GROUP BY 3", &lookup).expect_err("must refuse");
    assert_eq!(err.sqlstate(), "42P10");
    assert!(err
        .to_string()
        .contains("GROUP BY position 3 is not in select list"));
    // ORDER BY may name the key by position, alias, or expression.
    for sql in [
        "SELECT length(name) AS len, count(*) FROM t GROUP BY 1 ORDER BY 1 DESC",
        "SELECT length(name) AS len, count(*) FROM t GROUP BY len ORDER BY len DESC",
        "SELECT length(name) AS len, count(*) FROM t GROUP BY length(name) ORDER BY length(name) DESC",
    ] {
        match plan(sql, &lookup).unwrap_or_else(|e| panic!("{sql}: {e:?}")) {
            Statement::Aggregate(a) => {
                assert_eq!(a.order.len(), 1, "{sql}");
                assert_eq!(a.order[0].group_index, 0);
                assert!(!a.order[0].ascending);
            }
            other => panic!("wrong statement: {other:?}"),
        }
    }
}

/// `IS [NOT] NULL` over a constant, including PostgreSQL's row rule: a row is
/// null only when every field is, and not null only when none is.
#[test]
fn is_null_over_constants_matches_postgresql() {
    let sql = "SELECT row(null, null) IS NULL, row(null, null) IS NOT NULL, \
               row(1, null) IS NULL, row(1, null) IS NOT NULL, null IS NULL, \
               1 IS NOT NULL, '{}'::int[] IS NULL";
    match plan_ok(sql) {
        Statement::SelectConstant(sc) => {
            let values: Vec<Bson> = sc
                .columns
                .iter()
                .map(|(name, col, ty, _)| {
                    assert_eq!(name, "?column?");
                    assert_eq!(ty, "bool");
                    match col {
                        ConstCol::Value(v) => v.clone(),
                        other => panic!("not a value: {other:?}"),
                    }
                })
                .collect();
            assert_eq!(
                values,
                [true, false, false, false, true, true, false]
                    .map(Bson::Boolean)
                    .to_vec()
            );
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

/// A FROM-less `select unnest(array)` is one row per element in a column
/// named `unnest` of the element type; a NULL array is no rows at all. The
/// elements arrive as a text[] LITERAL, so the literal parser must keep the
/// quoted characters (`"`, `\`, `,`, `{`, `}`) intact.
#[test]
fn from_less_unnest_is_one_row_per_element() {
    match plan_ok(r#"SELECT unnest('{a,"b",",","\\","{","}",€}'::text[])"#) {
        Statement::ValuesConstant(vc) => {
            assert_eq!(vc.names, vec!["unnest".to_string()]);
            assert_eq!(vc.types, vec!["text".to_string()]);
            let got: Vec<&str> = vc.rows.iter().map(|r| r[0].as_str().unwrap()).collect();
            assert_eq!(got, ["a", "b", ",", "\\", "{", "}", "€"]);
        }
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT unnest('{1,2}'::int[]) AS u") {
        Statement::ValuesConstant(vc) => {
            assert_eq!(vc.names, vec!["u".to_string()]);
            assert_eq!(vc.types, vec!["int4".to_string()]);
            assert_eq!(vc.rows, vec![vec![Bson::Int32(1)], vec![Bson::Int32(2)]]);
        }
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT unnest(null::text[])") {
        Statement::ValuesConstant(vc) => {
            assert_eq!(vc.types, vec!["text".to_string()]);
            assert!(vc.rows.is_empty());
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
        (
            "BEGIN",
            TransactionControl::Begin(TransactionModes::default()),
        ),
        // `START TRANSACTION` is `BEGIN` with a different command tag, so it
        // is a different variant rather than the same one.
        (
            "START TRANSACTION",
            TransactionControl::Start(TransactionModes::default()),
        ),
        // The transaction characteristics are parsed onto the variant: the
        // isolation level and read-write mode were named, the deferrable mode
        // was not (so it stays `None` and inherits the session default).
        (
            "BEGIN ISOLATION LEVEL SERIALIZABLE READ WRITE",
            TransactionControl::Begin(TransactionModes {
                isolation: Some("serializable".into()),
                read_only: Some(false),
                deferrable: None,
            }),
        ),
        ("COMMIT", TransactionControl::Commit { chain: false }),
        ("ROLLBACK", TransactionControl::Rollback { chain: false }),
        // `AND CHAIN` ends the block and opens another, which the executor
        // cannot know unless the planner carries it.
        (
            "COMMIT AND CHAIN",
            TransactionControl::Commit { chain: true },
        ),
        (
            "ROLLBACK AND CHAIN",
            TransactionControl::Rollback { chain: true },
        ),
        // Two spellings PostgreSQL treats as COMMIT and ROLLBACK outright.
        ("END", TransactionControl::Commit { chain: false }),
        ("ABORT", TransactionControl::Rollback { chain: false }),
    ] {
        match plan_ok(sql) {
            Statement::Transaction(c) => assert_eq!(c, want, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
    // Savepoints are planned too -- a nested `conn.transaction()` block in any
    // client becomes one, and the name has to survive both spellings.
    for (sql, want) in [
        ("SAVEPOINT s1", TransactionControl::Savepoint("s1".into())),
        ("RELEASE s1", TransactionControl::Release("s1".into())),
        (
            "RELEASE SAVEPOINT s1",
            TransactionControl::Release("s1".into()),
        ),
        (
            "ROLLBACK TO s1",
            TransactionControl::RollbackTo("s1".into()),
        ),
        (
            "ROLLBACK TO SAVEPOINT s1",
            TransactionControl::RollbackTo("s1".into()),
        ),
    ] {
        match plan_ok(sql) {
            Statement::Transaction(c) => assert_eq!(c, want, "for {sql}"),
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// `SET TRANSACTION` and `SET SESSION CHARACTERISTICS AS TRANSACTION` are both
/// VAR_SET_MULTI in the parse tree, distinguished only by name; each carries the
/// characteristics as its own statement so the executor can tell "this block"
/// from "the session default".
#[test]
fn set_transaction_characteristics_are_planned() {
    match plan_ok("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY") {
        Statement::SetTransaction(m) => assert_eq!(
            m,
            TransactionModes {
                isolation: Some("repeatable read".into()),
                read_only: Some(true),
                deferrable: None,
            }
        ),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SET SESSION CHARACTERISTICS AS TRANSACTION ISOLATION LEVEL SERIALIZABLE") {
        Statement::SetSessionCharacteristics(m) => assert_eq!(
            m,
            TransactionModes {
                isolation: Some("serializable".into()),
                read_only: None,
                deferrable: None,
            }
        ),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE NOT DEFERRABLE") {
        Statement::SetSessionCharacteristics(m) => assert_eq!(
            m,
            TransactionModes {
                isolation: None,
                read_only: Some(false),
                deferrable: Some(false),
            }
        ),
        other => panic!("wrong statement: {other:?}"),
    }
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
                    "int4".to_string(),
                    -1
                )
            );
            assert_eq!(
                sc.columns[1],
                (
                    "current_database".to_string(),
                    ConstCol::CurrentDatabase,
                    "name".to_string(),
                    -1
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

/// An INSERT types an undeclared parameter from the column it goes into --
/// named when the statement lists columns, positional when it does not.
/// psycopg sends an empty `Multirange([])` untyped, so `insert into mr
/// values ($1, $2)` is the only thing that can say `$2` is an int4multirange.
#[test]
fn an_insert_types_a_parameter_from_its_column_by_name_or_position() {
    let column_type = |table: &str, column: ColumnRef<'_>| {
        assert_eq!(table, "mr");
        match column {
            ColumnRef::Name("id") | ColumnRef::Position(0) => Some("int4".to_string()),
            ColumnRef::Name("m") | ColumnRef::Position(1) => Some("int4multirange".to_string()),
            _ => None,
        }
    };
    let none = [None, None];
    assert_eq!(
        catalog_param_types_opt(
            "insert into mr (m, id) values ($1, $2)",
            &none,
            &column_type
        ),
        vec![Some("int4multirange".to_string()), Some("int4".to_string())]
    );
    assert_eq!(
        catalog_param_types_opt("insert into mr values ($1, $2)", &none, &column_type),
        vec![Some("int4".to_string()), Some("int4multirange".to_string())]
    );
    // A column the table does not have types nothing.
    assert_eq!(
        catalog_param_types_opt(
            "insert into mr values ($1, $2, $3)",
            &[None, None, None],
            &column_type
        ),
        vec![
            Some("int4".to_string()),
            Some("int4multirange".to_string()),
            None
        ]
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

/// Beyond 34 significant digits a numeric is stored as the wide-numeric
/// document, exactly -- never rounded -- and comes back as the same text.
///
/// PostgreSQL's `numeric` is arbitrary precision and Decimal128 is not; a
/// 35-digit value used to be refused (22003), and one whose EXTRA digit was a
/// trailing zero was silently rounded by `Decimal128::from_str`
/// (`100000000000000000.00000000000000000` lost a place of scale). Probed on
/// PostgreSQL 16, 2026-09-09.
#[test]
fn numeric_wider_than_decimal128_is_stored_exactly() {
    let value = |sql: &str| match plan_ok(sql) {
        Statement::SelectConstant(sc) => match &sc.columns[0].1 {
            ConstCol::Value(v) => v.clone(),
            other => panic!("{sql} should be a value, got {other:?}"),
        },
        other => panic!("wrong statement for {sql}: {other:?}"),
    };
    for (sql, want) in [
        (
            "SELECT '1.2345678901234567890123456789012345'::numeric",
            "1.2345678901234567890123456789012345",
        ),
        (
            "SELECT '999999999999999999.99999999999999999'::numeric",
            "999999999999999999.99999999999999999",
        ),
        (
            "SELECT '100000000000000000.00000000000000000'::numeric",
            "100000000000000000.00000000000000000",
        ),
        ("SELECT 1e40", "10000000000000000000000000000000000000000"),
        (
            "SELECT 1e40 + 0.5",
            "10000000000000000000000000000000000000000.5",
        ),
    ] {
        let v = value(sql);
        assert_eq!(numeric_text(&v).as_deref(), Some(want), "for {sql}");
    }
    assert_eq!(
        value("SELECT (1e40 + 0.5)::text"),
        Bson::String("10000000000000000000000000000000000000000.5".into())
    );
    // The wide form is the marker-key document; a value that fits stays a
    // Decimal128 (an integer with trailing zeros fits as `1E+40`).
    assert!(is_wide_numeric(&value(
        "SELECT '1.2345678901234567890123456789012345'::numeric"
    )));
    assert!(matches!(value("SELECT 1e40"), Bson::Decimal128(_)));
    // Not a number at all is a different code.
    let err = plan("SELECT 'x'::numeric", &lookup).expect_err("not numeric");
    assert_eq!(err.sqlstate(), "22P02");
    // Past PostgreSQL's own limits is its error, not a rounding.
    let err = plan("SELECT 1e131072", &lookup).expect_err("overflows numeric");
    assert_eq!(err.sqlstate(), "22003");
}

/// The canonical text rules, probed on PostgreSQL 16.
#[test]
fn numeric_canonical_text_matches_postgres() {
    for (input, want) in [
        ("0001.10", "1.10"),
        ("-0.0", "0.0"),
        ("-0.00e2", "0"),
        ("0.00e3", "0"),
        ("1.1e5", "110000"),
        ("1.1e-5", "0.000011"),
        ("100e-1", "10.0"),
        ("1.0e-3", "0.0010"),
        ("1.50e1", "15.0"),
        ("  12  ", "12"),
        ("1_000", "1000"),
        ("1_000.5", "1000.5"),
        ("+5", "5"),
        (".5", "0.5"),
        ("5.", "5"),
        ("nan", "NaN"),
        ("-inf", "-Infinity"),
        ("Infinity", "Infinity"),
    ] {
        assert_eq!(
            canonical_numeric_text(input).unwrap(),
            want,
            "for {input:?}"
        );
    }
    for bad in ["", "x", "1_", "_1", "1__0", "1e", "1.2.3", "--1"] {
        assert!(
            canonical_numeric_text(bad).is_err(),
            "{bad:?} should be invalid"
        );
    }
}

/// The sort key orders bytewise as the numbers order, across widths.
#[test]
fn numeric_sort_key_orders_like_the_numbers() {
    let texts = [
        "-Infinity",
        "-100000000000000000000000000000000000000",
        "-100",
        "-99.5",
        "-0.51",
        "-0.5",
        "-0.000000000000000000000000000000000000001",
        "0",
        "0.000000000000000000000000000000000000001",
        "0.5",
        "0.51",
        "99.5",
        "100",
        "100.0000000000000000000000000000000000001",
        "100000000000000000000000000000000000000",
        "Infinity",
        "NaN",
    ];
    let keys: Vec<String> = texts.iter().map(|t| numeric::numeric_sort_key(t)).collect();
    for w in keys.windows(2) {
        assert!(w[0] < w[1], "{} should sort before {}", w[0], w[1]);
    }
    // Scale is not part of the key: equal values share one key.
    assert_eq!(
        numeric::numeric_sort_key("1.50"),
        numeric::numeric_sort_key("1.5")
    );
    assert_eq!(
        numeric::numeric_sort_key("0.00"),
        numeric::numeric_sort_key("0")
    );
}

/// A wide constant lowers to an exact two-arm filter: the Decimal128 rows
/// compare against the constant's Decimal128 bracket, the wide rows against
/// the sort key.
#[test]
fn wide_numeric_where_lowers_to_bracket_and_key() {
    use numeric::{decimal128_bracket, Bracket};
    let def = TableDef::new(
        "w",
        vec![
            Column::new("id", "int4", true),
            Column::new("n", "numeric", false),
        ],
    );
    let lookup = |name: &str| (name == "w").then(|| def.clone());
    let filter = |sql: &str| match plan(sql, &lookup).expect("should plan") {
        Statement::Select(s) => s.filter,
        other => panic!("wrong statement for {sql}: {other:?}"),
    };
    // 35 digits: no Decimal128 equals it, so `=` has only the wide arm.
    let f = filter("SELECT id FROM w WHERE n = 1.2345678901234567890123456789012345");
    assert_eq!(
        f,
        doc! { "n.__numkey": { "$eq": numeric::numeric_sort_key("1.2345678901234567890123456789012345") } }
    );
    // `>` on a wide constant: every Decimal128 at or above the bracket's
    // upper neighbour, or a wide row above the key.
    let f = filter("SELECT id FROM w WHERE n > 1.2345678901234567890123456789012345");
    let Bracket::Between(lo, hi) =
        decimal128_bracket("1.2345678901234567890123456789012345").unwrap()
    else {
        panic!("35 digits should not be exact");
    };
    assert_eq!(lo.to_string(), "1.234567890123456789012345678901234");
    assert_eq!(hi.to_string(), "1.234567890123456789012345678901235");
    let nan = Bson::Decimal128("NaN".parse().unwrap());
    assert_eq!(
        f,
        doc! { "$or": [
            { "n": { "$gte": hi } },
            { "n.__numkey": { "$gt": numeric::numeric_sort_key("1.2345678901234567890123456789012345") } },
            { "n": &nan },
        ]}
    );
    // NaN sits ABOVE every number on PostgreSQL, where MQL's ranges exclude
    // it: `n < NaN` is every non-null, non-NaN row; `n > NaN` is no row.
    assert_eq!(
        filter("SELECT id FROM w WHERE n < 'NaN'::numeric"),
        doc! { "$and": [ { "n": { "$ne": &nan } }, { "n": { "$ne": Bson::Null } } ] }
    );
    assert_eq!(
        filter("SELECT id FROM w WHERE n > 'NaN'::numeric"),
        doc! { "n": { "$in": [] } }
    );
    assert_eq!(
        filter("SELECT id FROM w WHERE n >= 'NaN'::numeric"),
        doc! { "n": &nan }
    );
    // A narrow constant on a numeric column still gets the wide arm, because
    // the column may hold wide rows.
    let f = filter("SELECT id FROM w WHERE n < 5");
    assert_eq!(
        f,
        doc! { "$or": [
            { "n": { "$lt": Bson::Decimal128("5".parse().unwrap()) } },
            { "n.__numkey": { "$lt": numeric::numeric_sort_key("5") } },
        ]}
    );
    // A non-numeric column is lowered as before.
    assert_eq!(filter("SELECT id FROM w WHERE id = 5"), doc! { "_id": 5 });
    // The bracket of a value that fits by VALUE but not by scale is exact.
    assert!(matches!(
        decimal128_bracket("1.0000000000000000000000000000000000000000").unwrap(),
        Bracket::Exact(_)
    ));
    // Beyond Decimal128's exponent range the bracket is the finite ceiling.
    let Bracket::Between(lo, hi) = decimal128_bracket(&format!("1{}", "0".repeat(7000))).unwrap()
    else {
        panic!("1e7000 is not a Decimal128");
    };
    assert_eq!(lo.to_string(), "9.999999999999999999999999999999999E+6144");
    assert_eq!(hi.to_string(), "Infinity");
}

/// Arithmetic on wide numerics is exact, with PostgreSQL's result scales
/// (probed on 16, 2026-09-09) -- including division, which used to be
/// refused.
#[test]
fn wide_numeric_arithmetic_is_exact() {
    let calc = |sql: &str| match plan_ok(sql) {
        Statement::SelectConstant(sc) => match &sc.columns[0].1 {
            ConstCol::Value(v) => {
                numeric::numeric_operand_text(v).unwrap_or_else(|| panic!("{sql}: {v:?}"))
            }
            other => panic!("{sql} should be a value, got {other:?}"),
        },
        other => panic!("wrong statement for {sql}: {other:?}"),
    };
    for (sql, want) in [
        (
            "SELECT 99999999999999999999999999999999999 + 1",
            "100000000000000000000000000000000000",
        ),
        (
            "SELECT 1.2345678901234567890123456789012345 * 2",
            "2.4691357802469135780246913578024690",
        ),
        ("SELECT 1.50 / 3", "0.50000000000000000000"),
        ("SELECT 10.0 / 4", "2.5000000000000000"),
        ("SELECT 1 / 3.0", "0.33333333333333333333"),
        ("SELECT 2::numeric / 7", "0.28571428571428571429"),
        ("SELECT 0 / 3.0", "0.00000000000000000000"),
        ("SELECT 1000000 / 3.0", "333333.333333333333"),
        ("SELECT 1 / 8.0", "0.12500000000000000000"),
        ("SELECT 0.001 / 3", "0.00033333333333333333"),
        (
            "SELECT 123456789012345678901234567890123456789012345678901234567890 / 7",
            "17636684144620811271604938270017636684144620811271604938270",
        ),
        (
            "SELECT -1.2345678901234567890123456789012345",
            "-1.2345678901234567890123456789012345",
        ),
        (
            "SELECT abs(-1.2345678901234567890123456789012345)",
            "1.2345678901234567890123456789012345",
        ),
        (
            "SELECT round(1.2345678901234567890123456789012345, 2)",
            "1.23",
        ),
        ("SELECT 'NaN'::numeric + 1", "NaN"),
        ("SELECT 'Infinity'::numeric * 0", "NaN"),
        ("SELECT 'Infinity'::numeric * -2", "-Infinity"),
        ("SELECT 10 / 4", "2"),
    ] {
        assert_eq!(calc(sql), want, "for {sql}");
    }
    let err = plan("SELECT 1.5 / 0", &lookup).expect_err("division by zero");
    assert_eq!(err.sqlstate(), "22012");
    // Wide values compare exactly, across widths and against integers.
    use std::cmp::Ordering;
    let v = |t: &str| numeric_bson(t);
    assert_eq!(
        compare_constants(
            &v("100000000000000000000000000000000000000"),
            &v("100000000000000000000000000000000000000.0000000000000000000000000000001")
        ),
        Some(Ordering::Less)
    );
    assert_eq!(
        compare_constants(
            &v("1.0000000000000000000000000000000000000000"),
            &Bson::Int32(1)
        ),
        Some(Ordering::Equal)
    );
}

/// A datetime / interval ARRAY element renders as its scalar text, not as the
/// BSON value's Debug form. PostgreSQL 16: `array['2020-01-01 00:00:00.5'
/// ::timestamp]::text` is `{"2020-01-01 00:00:00.5"}`, `array['1 day'::
/// interval]::text` is `{"1 day"}`, `array['12:00'::time]::text` is
/// `{12:00:00}`. Before this the first two were `{"DateTime(2020-01-01
/// 0:00:00.5 +00:00:00)"}` and `{"Document({\"__ivl_mon\": ...})"}`.
#[test]
fn datetime_array_elements_render_as_their_scalar_text() {
    for (sql, want) in [
        (
            "SELECT ARRAY['2020-01-01 00:00:00.5'::timestamp]",
            "{\"2020-01-01 00:00:00.5\"}",
        ),
        ("SELECT ARRAY['1 day'::interval]", "{\"1 day\"}"),
        ("SELECT ARRAY['12:00'::time]", "{12:00:00}"),
        ("SELECT ARRAY['2020-01-01'::date]", "{2020-01-01}"),
    ] {
        match plan_ok(sql) {
            Statement::SelectConstant(sc) => {
                let ConstCol::Value(Bson::Array(items)) = &sc.columns[0].1 else {
                    panic!("{sql} should be an array, got {:?}", sc.columns[0].1);
                };
                assert_eq!(render_array(items), want, "for {sql}");
            }
            other => panic!("wrong statement for {sql}: {other:?}"),
        }
    }
}

/// The binary-wire helpers behind the datetime family, pinned to the bytes
/// PostgreSQL 16 sends (`binpin.py`, 2026-09-09): `date_send` is i32 days
/// since 2000-01-01 with the infinities at the i32 extremes and a BC date
/// counted back through the proleptic calendar; `timetz_send` is micros since
/// midnight plus the zone as seconds WEST of UTC; `timestamp_send` is i64
/// micros since 2000-01-01 with the infinities at the i64 extremes.
#[test]
fn datetime_binary_helpers_match_postgres() {
    assert_eq!(date_to_pg_days("2000-01-02"), Some(1));
    assert_eq!(date_to_pg_days("infinity"), Some(i32::MAX));
    assert_eq!(date_to_pg_days("-infinity"), Some(i32::MIN));
    assert_eq!(date_to_pg_days("0001-01-01 BC"), Some(-0x000b_2575)); // fff4da8b
    assert_eq!(date_to_pg_days("4713-01-01 BC"), Some(-0x0025_6833)); // ffda97cd
    assert_eq!(date_to_pg_days("not a date"), None);

    assert_eq!(
        timetz_to_pg_wire("12:00:00+05:30"),
        Some((43_200_000_000, -19_800))
    );
    assert_eq!(
        timetz_to_pg_wire("23:59:59.5-08"),
        Some((86_399_500_000, 28_800))
    );
    assert_eq!(timetz_to_pg_wire("12:00:00"), Some((43_200_000_000, 0)));

    assert_eq!(
        timestamp_text_to_pg_micros("2000-01-01 00:00:01"),
        Some(1_000_000)
    );
    // The values PostgreSQL sends in binary and psycopg's loader then refuses
    // ("timestamp too large" / "too small", hour 24): a wide year, a BC
    // timestamptz with its pass-through offset, and the end-of-day time.
    assert_eq!(
        timestamp_text_to_pg_micros("10000-01-01 12:00:00"),
        Some(0x0380_e715_a027_3000)
    );
    assert_eq!(
        timestamp_text_to_pg_micros("1000-01-01 12:00+00:00 BC"),
        Some(-0x0150_39e1_ac8b_1000)
    );
    assert_eq!(
        timestamp_text_to_pg_micros("2000-01-01 01:00:00+01"),
        Some(0)
    );
    assert_eq!(time_to_pg_micros("24:00:00"), Some(86_400_000_000));
    assert_eq!(time_to_pg_micros("24:00"), Some(86_400_000_000));
    assert_eq!(time_to_pg_micros("24:00:01"), None);
    assert_eq!(timestamp_text_to_pg_micros("infinity"), Some(i64::MAX));
    assert_eq!(timestamp_text_to_pg_micros("-infinity"), Some(i64::MIN));
    assert_eq!(
        timestamp_text_to_pg_micros("0001-01-01 00:00:00 BC"),
        Some(-0x00e0_39c2_e44e_e000) // ff1fc63d1bb12000
    );
    assert_eq!(timestamp_text_to_pg_micros("garbage"), None);
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
        Statement::CreateTable(ct, _) => {
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
                // A regtype VALUE now, rendered to its display name -- a plain
                // string could not also answer `::oid`. A type the catalog
                // cannot number (`unknown`) is still carried as its name.
                let rendered = match &sc.columns[0].1 {
                    ConstCol::Value(v) => match regtype_oid(v) {
                        Some(oid) => regtype_text(oid),
                        None => match v {
                            Bson::String(s) => s.clone(),
                            other => panic!("unexpected value for {sql}: {other:?}"),
                        },
                    },
                    other => panic!("not a value for {sql}: {other:?}"),
                };
                assert_eq!(rendered, want, "for {sql}");
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

/// Interval literals in every shape PostgreSQL accepts, rendered back the way
/// it renders them. Each pair was measured against PostgreSQL 14.
#[test]
fn interval_literals_round_trip() {
    for (literal, want) in [
        ("1 day", "1 day"),
        ("1 day 02:03:04", "1 day 02:03:04"),
        ("1d 3h 4m 5.678s", "1 day 03:04:05.678"),
        ("1 year 2 months", "1 year 2 mons"),
        ("P1Y2M3D", "1 year 2 mons 3 days"),
        ("PT1H2M3S", "01:02:03"),
        ("1 mon -1 day", "1 mon -1 days"),
        ("1.5 days", "1 day 12:00:00"),
        ("1 week", "7 days"),
        ("12 mons", "1 year"),
        ("13 mons", "1 year 1 mon"),
        ("0", "00:00:00"),
        // An interval's time part may exceed 24 hours; it is not a clock.
        ("25:00:00", "25:00:00"),
        ("0.5 sec", "00:00:00.5"),
        ("500 ms", "00:00:00.5"),
        ("1000 us", "00:00:00.001"),
        ("2 hrs 30 mins", "02:30:00"),
        // Negative values pluralise, which reads like a typo and is what
        // PostgreSQL emits.
        ("-1 day", "-1 days"),
        ("-1 mon", "-1 mons"),
        ("-13 mons", "-1 years -1 mons"),
        ("-1.5 hours", "-01:30:00"),
        // Independent signs: a positive day and a negative time.
        ("1 day -02:03:04", "1 day -02:03:04"),
    ] {
        let iv = super::parse_interval(literal).unwrap_or_else(|e| panic!("{literal}: {e:?}"));
        assert_eq!(super::render_interval(&iv), want, "for {literal}");
    }
}

/// Stripping a plural `s` from a unit must not eat the unit itself.
///
/// `trim_end_matches('s')` turned `s` (seconds) into the empty string and `ms`
/// (milliseconds) into `m` (minutes) — a factor of sixty thousand, and silent.
#[test]
fn interval_units_survive_depluralisation() {
    let us = |lit: &str| super::parse_interval(lit).expect("parses").micros;
    assert_eq!(us("5s"), 5_000_000);
    assert_eq!(us("5 s"), 5_000_000);
    assert_eq!(us("5 secs"), 5_000_000);
    assert_eq!(us("5 ms"), 5_000);
    assert_eq!(us("5 m"), 5 * 60_000_000);
    assert_eq!(us("5 mins"), 5 * 60_000_000);
    assert_eq!(us("5 us"), 5);
}

/// Intervals compare FLATTENED — 30-day months, 24-hour days — even though
/// they are stored as three parts. Probed: `'1 mon' = '30 days'` is true.
#[test]
fn intervals_compare_flattened() {
    let iv = |s: &str| super::parse_interval(s).expect("parses").to_bson();
    use std::cmp::Ordering;
    assert_eq!(
        super::compare_constants(&iv("1 day"), &iv("24:00:00")),
        Some(Ordering::Equal)
    );
    assert_eq!(
        super::compare_constants(&iv("1 mon"), &iv("30 days")),
        Some(Ordering::Equal)
    );
    assert_eq!(
        super::compare_constants(&iv("1 day"), &iv("25:00:00")),
        Some(Ordering::Less)
    );
    assert_eq!(
        super::compare_constants(&iv("1 day"), &iv("1 hour")),
        Some(Ordering::Greater)
    );
}

/// Adding months CLAMPS to the end of the target month, and that is why an
/// interval cannot be flattened for arithmetic: January 31st plus one month is
/// February 28th, which no number of microseconds expresses.
#[test]
fn adding_months_clamps_to_the_month_end() {
    let at = |t: &str| super::parse_timestamp(t).expect("parses");
    let add = |t: &str, i: &str| {
        let iv = super::parse_interval(i).expect("parses");
        super::render_timestamp(super::add_interval_to_micros(at(t), &iv, 1).expect("in range"))
    };
    assert_eq!(add("2026-01-31 00:00:00", "1 mon"), "2026-02-28 00:00:00");
    assert_eq!(add("2026-01-31 00:00:00", "2 mons"), "2026-03-31 00:00:00");
    assert_eq!(add("2024-02-29 00:00:00", "1 year"), "2025-02-28 00:00:00");
    // Days and time are added AFTER the month shift, and are exact.
    assert_eq!(
        add("2026-01-01 00:00:00", "1d 3h 4m 5.678s"),
        "2026-01-02 03:04:05.678"
    );
}

/// Scaling an interval spills fractions DOWNWARD — months into days, days into
/// time — because a fraction of a month has no calendar meaning even though a
/// whole one does. `'1 mon' * 1.5` is `1 mon 15 days`, not `1.5 mons`.
#[test]
fn scaling_an_interval_spills_downward() {
    let scale = |lit: &str, op: &str, n: Bson| {
        let iv = super::parse_interval(lit).expect("parses").to_bson();
        let out = super::eval_binary(op, iv, n).expect("scales");
        super::render_interval(&Interval::from_bson(&out).expect("an interval"))
    };
    assert_eq!(scale("1 day", "*", Bson::Int32(2)), "2 days");
    assert_eq!(scale("1 day", "*", Bson::Double(0.5)), "12:00:00");
    assert_eq!(scale("1 mon", "*", Bson::Double(1.5)), "1 mon 15 days");
    assert_eq!(scale("1 year", "*", Bson::Double(0.5)), "6 mons");
    assert_eq!(scale("1 day", "/", Bson::Int32(2)), "12:00:00");
    assert_eq!(scale("1 mon 1 day", "*", Bson::Int32(2)), "2 mons 2 days");

    // Dividing by zero is an error, not an infinity.
    let iv = super::parse_interval("1 day").expect("parses").to_bson();
    let err = super::eval_binary("/", iv, Bson::Int32(0)).expect_err("div by zero");
    assert_eq!(err.sqlstate(), "22012");
}

/// `numeric` arithmetic is EXACT, and its result SCALE is part of the answer.
///
/// Measured on PostgreSQL 14: addition and subtraction take `max(s1, s2)`,
/// multiplication takes `s1 + s2`. So `1.50 + 1.5` is `3.00`, not `3.0`, and
/// `1.50 * 1.50` is `2.2500`. None of this survives a trip through an `f64`.
///
/// This is a REGRESSION test in the strict sense: when decimal literals became
/// `numeric` rather than floats, every one of these operators started refusing
/// outright — `1.5 + 1.5` was an error — and no test caught it.
#[test]
fn decimal_arithmetic_is_exact_and_keeps_its_scale() {
    let calc = |sql: &str| match plan_ok(sql) {
        Statement::SelectConstant(sc) => match &sc.columns[0].1 {
            ConstCol::Value(Bson::Decimal128(d)) => d.to_string(),
            other => panic!("{sql} should be a decimal, got {other:?}"),
        },
        other => panic!("wrong statement for {sql}: {other:?}"),
    };
    for (sql, want) in [
        ("SELECT 1.5 + 1.5", "3.0"),
        ("SELECT 1.50 + 1.5", "3.00"),
        ("SELECT 1 + 1.5", "2.5"),
        ("SELECT 1.234 + 1.1", "2.334"),
        ("SELECT 2.00 - 1.0", "1.00"),
        ("SELECT 2.5 * 2", "5.0"),
        ("SELECT 2.5 * 2.0", "5.00"),
        ("SELECT 1.50 * 1.50", "2.2500"),
        ("SELECT 0.1 * 0.1", "0.01"),
        // The reason exactness matters at all.
        ("SELECT 0.1 + 0.2", "0.3"),
        ("SELECT -1.5", "-1.5"),
    ] {
        assert_eq!(calc(sql), want, "for {sql}");
    }
}

/// Decimals compare on their DIGITS, not through a float.
///
/// A `numeric` holds 34 significant digits and an `f64` holds about 15, so two
/// different 20-digit numbers are the SAME float. Scale is not part of
/// equality — `1.50 = 1.5` — but precision is.
#[test]
fn decimals_compare_exactly() {
    use std::cmp::Ordering;
    let d = |t: &str| Bson::Decimal128(t.parse().expect("a decimal"));
    assert_eq!(
        super::compare_constants(&d("1.50"), &d("1.5")),
        Some(Ordering::Equal)
    );
    assert_eq!(
        super::compare_constants(&d("0"), &d("-0")),
        Some(Ordering::Equal)
    );
    // Identical as f64, different as numerics.
    assert_eq!(
        super::compare_constants(&d("12345678901234567890.1"), &d("12345678901234567890.2")),
        Some(Ordering::Less)
    );
    assert_eq!(
        super::compare_constants(&d("-1.5"), &d("-1.4")),
        Some(Ordering::Less)
    );
    // A magnitude Decimal128 renders in exponent form (`-8.34184E-7`) is
    // still a number: the digit comparison used to see the `E` and give up,
    // which reached psycopg as "comparing numeric range bounds is not
    // supported yet" on any `numrange` with a small enough bound.
    assert_eq!(
        super::compare_constants(&d("-8.34184E-7"), &d("1")),
        Some(Ordering::Less)
    );
    assert_eq!(
        super::compare_constants(&d("1.5E+20"), &d("150000000000000000000")),
        Some(Ordering::Equal)
    );
    assert_eq!(
        super::compare_constants(&d("8.34184E-7"), &d("0.000000834184")),
        Some(Ordering::Equal)
    );
    assert_eq!(
        super::compare_constants(&d("8.34184E-7"), &d("0.000000834185")),
        Some(Ordering::Less)
    );
}

/// PostgreSQL gives NaN a place in a TOTAL order, which IEEE does not: NaN
/// equals itself and sorts ABOVE every number, infinity included.
///
/// `f64::partial_cmp` reports every NaN comparison as `None`, which this server
/// turned into "cannot compare" — an error where PostgreSQL has an answer.
#[test]
fn nan_sorts_above_everything_and_equals_itself() {
    use std::cmp::Ordering;
    let f = |v: f64| Bson::Double(v);
    assert_eq!(
        super::compare_constants(&f(f64::NAN), &f(f64::NAN)),
        Some(Ordering::Equal)
    );
    assert_eq!(
        super::compare_constants(&f(f64::NAN), &f(f64::INFINITY)),
        Some(Ordering::Greater)
    );
    assert_eq!(
        super::compare_constants(&f(f64::INFINITY), &f(1e308)),
        Some(Ordering::Greater)
    );
    assert_eq!(
        super::compare_constants(&f(f64::NEG_INFINITY), &f(-1e308)),
        Some(Ordering::Less)
    );
    // A decimal NaN follows the same rule.
    let d = |t: &str| Bson::Decimal128(t.parse().expect("a decimal"));
    assert_eq!(
        super::compare_constants(&d("NaN"), &d("NaN")),
        Some(Ordering::Equal)
    );
}

/// `json` keeps what it was given; `jsonb` normalises. Every pair measured
/// against PostgreSQL 14.
#[test]
fn jsonb_normalises_where_json_preserves() {
    let jsonb = |t: &str| {
        let v = crate::json::parse(t).expect("valid json");
        crate::json::render_jsonb(&v)
    };
    for (input, want) in [
        // Keys sort by BYTE LENGTH first, then bytewise — not lexicographically.
        (r#"{"bb":1,"a":2}"#, r#"{"a": 2, "bb": 1}"#),
        (r#"{"aa":1,"ab":2,"b":3}"#, r#"{"b": 3, "aa": 1, "ab": 2}"#),
        // `z` is one byte and `é` is two, so `z` sorts first.
        (r#"{"é":1,"z":2}"#, r#"{"z": 2, "é": 1}"#),
        // Bytewise, so uppercase precedes lowercase.
        (r#"{"b":1,"A":2}"#, r#"{"A": 2, "b": 1}"#),
        // The LAST of a duplicate pair wins.
        (r#"{"a":1, "a":2}"#, r#"{"a": 2}"#),
        // Whitespace is canonical, and nesting is normalised too.
        ("  {\"a\" : 1 }  ", r#"{"a": 1}"#),
        (r#"[1,  2,   3]"#, "[1, 2, 3]"),
        (
            r#"{"nested": {"z":1,"a":2}}"#,
            r#"{"nested": {"a": 2, "z": 1}}"#,
        ),
        (r#"{}"#, "{}"),
        (r#"[]"#, "[]"),
    ] {
        assert_eq!(jsonb(input), want, "for {input}");
    }
}

/// A `jsonb` number is a `numeric`, and prints the way one does.
///
/// So the exponent is expanded, but a trailing zero written in the literal
/// SURVIVES — it is the value's scale. Any parser that routes numbers through
/// an `f64` loses the second half.
#[test]
fn jsonb_numbers_print_as_numerics() {
    let jsonb = |t: &str| {
        let v = crate::json::parse(t).expect("valid json");
        crate::json::render_jsonb(&v)
    };
    for (input, want) in [
        (r#"{"x": 1.10}"#, r#"{"x": 1.10}"#),
        (r#"{"n":-1.5e10}"#, r#"{"n": -15000000000}"#),
        (r#"{"n":1e3}"#, r#"{"n": 1000}"#),
        (r#"{"n":1.5E-3}"#, r#"{"n": 0.0015}"#),
        (r#"{"n":0.0}"#, r#"{"n": 0.0}"#),
        (r#"{"n":100}"#, r#"{"n": 100}"#),
        (
            r#"{"big":123456789012345678901234567890}"#,
            r#"{"big": 123456789012345678901234567890}"#,
        ),
    ] {
        assert_eq!(jsonb(input), want, "for {input}");
    }
}

/// Malformed JSON is refused. `01` matters beyond JSON: it is the case that
/// showed parameter sniffing was making a value MORE acceptable than the
/// client wrote it.
#[test]
fn malformed_json_is_refused() {
    for bad in [
        "{bad}",
        r#"{"a":}"#,
        "[1,]",
        "01",
        r#"{"a":1} x"#,
        "",
        r#"{"a" 1}"#,
        "[1 2]",
        r#""unterminated"#,
        "{",
        "tru",
    ] {
        assert!(crate::json::parse(bad).is_err(), "{bad:?} should not parse");
    }
}

/// `\uXXXX` escapes, measured against PostgreSQL 16's `jsonb` input: a
/// surrogate PAIR is one character, either half alone is a 22P02, and
/// `\u0000` is the distinct 22P05 (it decodes, but to a NUL text cannot
/// hold). The `json` type keeps every escape verbatim, which the cast
/// handles by tolerating the last two errors.
#[test]
fn json_unicode_escapes_match_postgres() {
    use crate::json::{parse, Json, ParseError};
    assert_eq!(
        parse(r#""\ud83d\ude00""#),
        Ok(Json::Str("\u{1F600}".into()))
    );
    assert_eq!(parse(r#""\u00e9""#), Ok(Json::Str("\u{e9}".into())));
    assert_eq!(parse(r#""\u0041\u00000""#), Err(ParseError::NulEscape));
    assert_eq!(parse(r#""\u0000""#), Err(ParseError::NulEscape));
    for lone in [
        r#""\ud83d""#,
        r#""\ude00""#,
        r#""\ud83dx""#,
        r#""\ud83d\ud83d""#,
        r#"{"a":"\ud83d"}"#,
    ] {
        assert_eq!(parse(lone), Err(ParseError::UnpairedSurrogate), "{lone}");
    }
    assert_eq!(parse(r#""\u12g4""#), Err(ParseError::Syntax));
    assert_eq!(parse(r#""\u12""#), Err(ParseError::Syntax));

    let cast = |t: &str, lit: &str| match plan_ok(&format!("SELECT '{lit}'::{t}::text")) {
        Statement::SelectConstant(sc) => match &sc.columns[0].1 {
            ConstCol::Value(Bson::String(s)) => s.clone(),
            other => panic!("{lit} -> {other:?}"),
        },
        other => panic!("wrong statement for {lit}: {other:?}"),
    };
    assert_eq!(cast("jsonb", r#""\ud83d\ude00""#), "\"\u{1F600}\"");
    assert_eq!(cast("json", r#""\ud83d""#), r#""\ud83d""#);
    assert_eq!(cast("json", r#""\u0000""#), r#""\u0000""#);
    let err = plan(r#"SELECT '"\ud83d"'::jsonb"#, &lookup).unwrap_err();
    assert_eq!(err.sqlstate(), "22P02");
    assert_eq!(err.to_string(), "invalid input syntax for type json");
    let err = plan(r#"SELECT '"\u0000"'::jsonb"#, &lookup).unwrap_err();
    assert_eq!(err.sqlstate(), "22P05");
    assert_eq!(err.to_string(), "unsupported Unicode escape sequence");
}

/// The scalar built-ins, every case measured against PostgreSQL 14.
///
/// Types are as much of the answer as values: `length` gives `int4`, `exp`
/// gives `float8`, `abs` gives back what it was handed. And the two rounding
/// families disagree — `round` on a `numeric` goes half AWAY FROM ZERO while on
/// a `float8` it goes half TO EVEN, the same split the integer casts have.
#[test]
fn scalar_builtins_match_postgres() {
    let calc = |sql: &str| match plan_ok(&format!("SELECT ({sql})::text")) {
        Statement::SelectConstant(sc) => match &sc.columns[0].1 {
            ConstCol::Value(Bson::String(s)) => s.clone(),
            ConstCol::Value(Bson::Null) => "NULL".to_string(),
            other => panic!("{sql} -> {other:?}"),
        },
        other => panic!("wrong statement for {sql}: {other:?}"),
    };
    for (expr, want) in [
        // strings
        ("upper('aB')", "AB"),
        ("lower('Ab')", "ab"),
        ("initcap('ab cd')", "Ab Cd"),
        ("btrim('xxaxx','x')", "a"),
        ("substr('abcdef',2,3)", "bcd"),
        ("replace('abcabc','b','X')", "aXcaXc"),
        ("repeat('ab',3)", "ababab"),
        ("reverse('abc')", "cba"),
        // A negative count means "all but this many from the other end".
        ("left('abcde',-2)", "abc"),
        ("right('abcde',-2)", "cde"),
        ("split_part('a,b,c',',',2)", "b"),
        ("md5('a')", "0cc175b9c0f1b6a831c399e269772661"),
        // `length` counts CHARACTERS and `octet_length` counts BYTES; they
        // differ the moment the text stops being ASCII.
        ("length('héllo')", "5"),
        ("octet_length('héllo')", "6"),
        ("chr(233)", "é"),
        ("ascii('é')", "233"),
        ("strpos('abcabc','c')", "3"),
        ("strpos('abc','z')", "0"),
        // `concat` SKIPS nulls rather than propagating them.
        ("concat('a',null,'b')", "ab"),
        ("concat_ws('-','a',null,'b')", "a-b"),
        // numbers
        ("abs(-5.5)", "5.5"),
        ("sign(-3)", "-1"),
        ("ceil(-1.2)", "-1"),
        ("floor(-1.7)", "-2"),
        ("trunc(-1.9)", "-1"),
        ("round(1.234,2)", "1.23"),
        ("trunc(1.999,2)", "1.99"),
        ("div(7,3)", "2"),
        ("mod(-7,3)", "-1"),
        ("power(2,3)", "8"),
        ("log(100)", "2"),
        // numeric rounds half AWAY FROM ZERO...
        ("round(1.5)", "2"),
        ("round(2.5)", "3"),
        ("round(-1.5)", "-2"),
        // ...and float8 rounds half TO EVEN.
        ("round(1.5::float8)", "2"),
        ("round(2.5::float8)", "2"),
        // conditionals: greatest/least IGNORE nulls, unlike everything else
        ("greatest(1,2,3)", "3"),
        ("least(3,2,1)", "1"),
        ("greatest(1,null)", "1"),
        ("greatest(null,null)", "NULL"),
        ("coalesce(null,1)", "1"),
        ("coalesce(null,null)", "NULL"),
        ("nullif(1,1)", "NULL"),
        ("nullif(1,2)", "1"),
        // `div` is defined on numeric, so integer arguments coerce and the
        // answer is a numeric — not the int8 the arithmetic suggests.
        ("div(7,3)", "2"),
        ("sign(-2.5)", "-1"),
    ] {
        assert_eq!(calc(expr), want, "for {expr}");
    }
}

/// A built-in's RESULT TYPE is as much of the answer as its value.
///
/// `sign` answers `float8` even for an integer argument; `nullif` answers its
/// LEFT operand's type even when the result is NULL — and a NULL cannot report
/// a type, so reading it from the value gave `text` where PostgreSQL says
/// `int4`. A literal carries its type in its own node.
#[test]
fn scalar_builtins_report_postgres_result_types() {
    let ty = |sql: &str| match plan_ok(&format!("SELECT {sql}")) {
        Statement::SelectConstant(sc) => sc.columns[0].2.clone(),
        other => panic!("wrong statement for {sql}: {other:?}"),
    };
    for (expr, want) in [
        ("length('abc')", "int4"),
        ("ascii('A')", "int4"),
        ("upper('a')", "text"),
        ("md5('a')", "text"),
        ("exp(1)", "float8"),
        ("sqrt(4)", "float8"),
        ("sign(-3)", "float8"),
        ("div(7,3)", "numeric"),
        ("starts_with('abc','ab')", "bool"),
        // NULL results still carry the type of what they came from.
        ("nullif(1,1)", "int4"),
        ("nullif(1.5,1.5)", "numeric"),
        ("nullif('a','a')", "text"),
    ] {
        assert_eq!(ty(expr), want, "for {expr}");
    }
}

/// A NULL argument gives a NULL answer for the propagating majority.
#[test]
fn scalar_builtins_propagate_null() {
    for expr in ["upper(null)", "length(null)", "abs(null)", "round(null)"] {
        match plan_ok(&format!("SELECT {expr}")) {
            Statement::SelectConstant(sc) => {
                assert_eq!(sc.columns[0].1, ConstCol::Value(Bson::Null), "for {expr}");
            }
            other => panic!("wrong statement for {expr}: {other:?}"),
        }
    }
}

/// A range over a DISCRETE element type has exactly one spelling.
///
/// PostgreSQL rewrites every bound to `[)`, so `'[1,5]'` is stored and printed
/// as `[1,6)` and `'(1,5)'` as `[2,5)`. Over a CONTINUOUS type there is no such
/// rewrite, because there is no "next" number to move a bound to — so
/// `'[1.0,2.0]'::numrange` stays inclusive.
///
/// Getting that split wrong makes two spellings of one range compare unequal.
#[test]
fn discrete_ranges_canonicalise_and_continuous_ones_do_not() {
    let render = |sql: &str, ty: &str| {
        let r = crate::range::from_text(sql, ty).unwrap_or_else(|e| panic!("{sql}: {e:?}"));
        crate::range::render(&r)
    };
    // int4range is discrete.
    assert_eq!(render("[1,5)", "int4range"), "[1,5)");
    assert_eq!(render("[1,5]", "int4range"), "[1,6)");
    assert_eq!(render("(1,5)", "int4range"), "[2,5)");
    assert_eq!(render("(1,5]", "int4range"), "[2,6)");
    // daterange steps by whole days.
    assert_eq!(
        render("[2026-01-01,2026-01-05]", "daterange"),
        "[2026-01-01,2026-01-06)"
    );
    // numrange is continuous: the bounds are left exactly as written.
    assert_eq!(render("[1.0,2.0]", "numrange"), "[1.0,2.0]");
    assert_eq!(render("(1.0,2.0)", "numrange"), "(1.0,2.0)");
    // An infinite bound prints as nothing at all.
    assert_eq!(render("(,5)", "int4range"), "(,5)");
    assert_eq!(render("[1,)", "int4range"), "[1,)");
    assert_eq!(render("(,)", "int4range"), "(,)");
    // A range that contains nothing IS empty, however it was written.
    assert_eq!(render("[1,1)", "int4range"), "empty");
    assert_eq!(render("empty", "int4range"), "empty");
}

/// Two spellings of one range are the same range, which is what
/// canonicalisation is for.
#[test]
fn equal_ranges_render_identically() {
    let render = |sql: &str, ty: &str| {
        crate::range::render(&crate::range::from_text(sql, ty).expect("valid"))
    };
    assert_eq!(render("[1,5]", "int4range"), render("[1,6)", "int4range"));
    assert_eq!(render("(0,5)", "int4range"), render("[1,5)", "int4range"));
}

/// A bound needs quoting when its text would be ambiguous inside the brackets.
/// A timestamp always does — it has a space in the middle.
#[test]
fn range_bounds_are_quoted_when_ambiguous() {
    let r = crate::range::from_text("[2026-01-01 00:00:00,2026-01-02 00:00:00)", "tsrange")
        .expect("valid");
    assert_eq!(
        crate::range::render(&r),
        "[\"2026-01-01 00:00:00\",\"2026-01-02 00:00:00\")"
    );
}

/// A crossed bound is a DATA error (22000), while a malformed literal is an
/// invalid-text one (22P02) and bad bound flags are a syntax error (42601).
/// Three different classes for three different mistakes.
#[test]
fn range_errors_carry_postgres_classes() {
    let crossed = crate::range::from_text("[5,1)", "int4range").expect_err("crossed");
    assert_eq!(crossed.sqlstate(), "22000");
    let malformed = crate::range::from_text("x", "int4range").expect_err("malformed");
    assert_eq!(malformed.sqlstate(), "22P02");
    let flags = crate::range::from_args(
        &[Bson::Int32(1), Bson::Int32(5), Bson::String("x".into())],
        "int4range",
        true,
    )
    .expect_err("bad flags");
    assert_eq!(flags.sqlstate(), "42601");

    // A literal NULL for the flags is a data error; the same NULL arriving
    // from a not-yet-bound parameter is not, because Describe runs before Bind.
    let null_literal = crate::range::from_args(
        &[Bson::Int32(1), Bson::Int32(5), Bson::Null],
        "int4range",
        true,
    )
    .expect_err("null flags");
    assert_eq!(null_literal.sqlstate(), "22000");
    assert!(crate::range::from_args(
        &[Bson::Int32(1), Bson::Int32(5), Bson::Null],
        "int4range",
        false,
    )
    .is_ok());
}

/// A multirange is a NORMALISED set of ranges: empties dropped, the rest
/// sorted, and any two that overlap **or merely touch** merged into one.
///
/// Adjacency is the part that is easy to miss. `{[1,5),[5,8)}` is `{[1,8)}`
/// because nothing lies between them, while `{[1,5),[6,8)}` stays two members
/// because 5 does. So the test is "does the next one start at or before this
/// one ends", not "do they overlap". Every case measured on PostgreSQL 14.
#[test]
fn multiranges_merge_what_touches() {
    let mr = |text: &str, ty: &str| {
        let members = crate::range::multirange_from_text(text, ty)
            .unwrap_or_else(|e| panic!("{text}: {e:?}"));
        crate::range::render_multirange(&members)
    };
    for (input, want) in [
        ("{[1,5)}", "{[1,5)}"),
        // sorted
        ("{[10,20),[1,5)}", "{[1,5),[10,20)}"),
        // overlapping
        ("{[1,5),[3,8)}", "{[1,8)}"),
        // touching, so merged
        ("{[1,5),[5,8)}", "{[1,8)}"),
        // a gap at 5, so kept apart
        ("{[1,5),[6,8)}", "{[1,5),[6,8)}"),
        // chains collapse
        ("{[1,2),[2,3),[3,4)}", "{[1,4)}"),
        // wholly contained
        ("{[1,5),[2,3)}", "{[1,5)}"),
        // empties are dropped, including all of them
        ("{}", "{}"),
        ("{empty}", "{}"),
        ("{[1,5),empty,[10,20)}", "{[1,5),[10,20)}"),
        // members canonicalise first, so this is [1,6)
        ("{[1,5]}", "{[1,6)}"),
        // infinite bounds
        ("{(,5)}", "{(,5)}"),
        ("{(,5),[10,)}", "{(,5),[10,)}"),
    ] {
        assert_eq!(mr(input, "int4multirange"), want, "for {input}");
    }
    // A CONTINUOUS element type has no adjacency by stepping, so touching
    // depends entirely on the bounds: `[2.0` closes the gap that `(2.0` leaves.
    assert_eq!(mr("{[1.0,2.0),[2.0,3.0)}", "nummultirange"), "{[1.0,3.0)}");
    assert_eq!(
        mr("{[1.0,2.0),(2.0,3.0)}", "nummultirange"),
        "{[1.0,2.0),(2.0,3.0)}"
    );
}

/// A multirange literal is split on brackets, not on every comma — its members
/// contain commas of their own.
#[test]
fn malformed_multiranges_are_refused() {
    for bad in ["{[1,5)", "{x}", "[1,5)", "{[1,5)},", "{{}}"] {
        assert!(
            crate::range::multirange_from_text(bad, "int4multirange").is_err(),
            "{bad:?} should not parse"
        );
    }
}

/// `generate_series` counts by its step, which may be negative — and a range
/// that runs the wrong way for its step is EMPTY rather than reversed.
#[test]
fn generate_series_counts_by_its_step() {
    let vals = |start: i64, stop: i64, step: i64| {
        crate::Series {
            start,
            stop,
            step,
            column: "generate_series".into(),
        }
        .values()
    };
    assert_eq!(vals(1, 5, 1), vec![1, 2, 3, 4, 5]);
    assert_eq!(vals(1, 10, 3), vec![1, 4, 7, 10]);
    assert_eq!(vals(5, 1, -2), vec![5, 3, 1]);
    // Counting up towards a smaller stop produces nothing; it does not reverse.
    assert_eq!(vals(5, 1, 1), Vec::<i64>::new());
    assert_eq!(vals(1, 0, 1), Vec::<i64>::new());
    assert_eq!(vals(3, 3, 1), vec![3]);
    // A zero step never terminates, which is why PostgreSQL refuses it before
    // it gets this far.
    assert_eq!(vals(1, 5, 0), Vec::<i64>::new());
}

/// A zero step is `22023` — the argument is a number of the right shape whose
/// VALUE cannot work, which PostgreSQL separates from its generic data class.
#[test]
fn generate_series_rejects_a_zero_step() {
    let err = plan("SELECT * FROM generate_series(1,5,0)", &lookup).expect_err("zero step");
    assert_eq!(err.sqlstate(), "22023");
    assert_eq!(err.to_string(), "step size cannot equal zero");
}

/// The FROM item's alias renames the generated column, and a column alias
/// beats the table one.
#[test]
fn a_series_alias_renames_its_column() {
    let column = |sql: &str| match plan_ok(sql) {
        Statement::Select(sel) => {
            let series = sel.series.expect("a series");
            (series.column, sel.columns)
        }
        other => panic!("wrong statement for {sql}: {other:?}"),
    };
    let (col, out) = column("SELECT * FROM generate_series(1,3)");
    assert_eq!(col, "generate_series");
    assert_eq!(
        out,
        vec![("generate_series".to_string(), "generate_series".to_string())]
    );

    let (col, _) = column("SELECT * FROM generate_series(1,3) AS g");
    assert_eq!(col, "g");

    // `AS g(x)` — the COLUMN alias wins over the table alias.
    let (col, _) = column("SELECT * FROM generate_series(1,3) AS g(x)");
    assert_eq!(col, "x");
}

/// A WHERE clause over a generated source becomes the series' filter (it
/// used to be refused with `0A000`; PostgreSQL 16 answers 3 rows here).
#[test]
fn a_where_over_a_series_filters_it() {
    let planned = plan(
        "SELECT * FROM generate_series(1,5) WHERE generate_series > 2",
        &lookup,
    )
    .expect("where over a series");
    match planned {
        Statement::Select(sel) => {
            assert!(sel.series.is_some());
            assert_eq!(sel.filter, bson::doc! { "generate_series": { "$gt": 2 } });
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

/// A set-returning function in the SELECT LIST of a FROM-less query is not a
/// constant: `select generate_series(1,3)` is three ROWS.
///
/// It is planned as an ordinary select over a generated source — the same shape
/// `FROM generate_series(...)` produces — so ORDER BY, LIMIT and OFFSET keep
/// working without a second implementation of any of them.
#[test]
fn a_set_returning_function_in_the_select_list_makes_rows() {
    match plan_ok("SELECT generate_series(1,3)") {
        Statement::Select(sel) => {
            let series = sel.series.expect("a series");
            assert_eq!(series.values(), vec![1, 2, 3]);
            assert_eq!(series.column, "generate_series");
            assert_eq!(
                sel.columns,
                vec![("generate_series".to_string(), "generate_series".to_string())]
            );
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // The alias renames the output column.
    match plan_ok("SELECT generate_series(1,3) AS g") {
        Statement::Select(sel) => {
            assert_eq!(sel.series.expect("a series").column, "g");
        }
        other => panic!("wrong statement: {other:?}"),
    }
    // ORDER BY and LIMIT come along.
    match plan_ok("SELECT generate_series(1,10) ORDER BY 1 DESC LIMIT 3") {
        Statement::Select(sel) => {
            assert_eq!(sel.limit, Some(3));
            assert_eq!(sel.order.len(), 1);
            assert!(!sel.order[0].ascending);
        }
        other => panic!("wrong statement: {other:?}"),
    }
}

/// A set-returning function BESIDE another output column is refused.
///
/// `select 1, generate_series(1,3)` repeats the constant across the generated
/// rows, which needs the constants carried into each row. Nothing in the corpus
/// asks for it, and a shape that silently dropped a column would be worse than
/// saying so.
#[test]
fn a_set_returning_function_beside_a_column_is_refused() {
    let err = plan("SELECT 1, generate_series(1,3)", &lookup).expect_err("srf beside a column");
    assert_eq!(err.sqlstate(), "0A000");
}

#[test]
fn a_numeric_never_renders_in_exponent_notation() {
    // PostgreSQL's numeric output is always plain; Decimal128's is not, and
    // the difference reached the wire, `::text` and array elements alike.
    assert_eq!(plain_numeric_text("1.5E+20"), "150000000000000000000");
    assert_eq!(plain_numeric_text("2E+3"), "2000");
    assert_eq!(plain_numeric_text("1E-10"), "0.0000000001");
    assert_eq!(plain_numeric_text("1.5E-5"), "0.000015");
    assert_eq!(plain_numeric_text("-1.5E+3"), "-1500");
    // The scale is part of the value, so a plain rendering is left alone.
    assert_eq!(plain_numeric_text("1.50"), "1.50");
    assert_eq!(plain_numeric_text("0"), "0");
    // Non-finite values carry no exponent to expand.
    assert_eq!(plain_numeric_text("NaN"), "NaN");
    assert_eq!(plain_numeric_text("Infinity"), "Infinity");
    // Anything that is not a decimal at all is passed through untouched
    // rather than mangled into one.
    assert_eq!(plain_numeric_text("elephant"), "elephant");
}

/// The bounds of a series arrive as PARAMETERS far more often than as
/// literals, and an untyped parameter arrives as TEXT.
///
/// Every answer here was taken from PostgreSQL 14: a text bound is read as an
/// integer, a NULL bound makes an EMPTY series rather than an error, and a
/// `float8` bound matches no overload there -- this server used to truncate
/// one, which is a wrong answer where a real server refuses.
#[test]
fn a_series_reads_its_bounds_from_parameters() {
    let series = |params: &[Bson]| -> Series {
        match plan_with_params("SELECT * FROM generate_series(1, $1)", &lookup, params) {
            Ok(Statement::Select(sel)) => sel.series.expect("a series"),
            other => panic!("wrong statement: {other:?}"),
        }
    };
    assert_eq!(
        series(&[Bson::String("4".into())]).values(),
        vec![1, 2, 3, 4]
    );
    // Whitespace around a bound is accepted, as PostgreSQL's integer parser is.
    assert_eq!(
        series(&[Bson::String(" 3 ".into())]).values(),
        vec![1, 2, 3]
    );
    // A NULL bound: zero rows, not an error.
    assert!(series(&[Bson::Null]).values().is_empty());

    let err = plan_with_params(
        "SELECT * FROM generate_series(1, $1)",
        &lookup,
        &[Bson::String("x".into())],
    )
    .expect_err("not an integer");
    assert_eq!(err.sqlstate(), "22P02");
    assert_eq!(
        err.to_string(),
        "invalid input syntax for type integer: \"x\""
    );

    // There is no `generate_series(int, float8)` in PostgreSQL.
    let err = plan("SELECT * FROM generate_series(1, 3::float8)", &lookup)
        .expect_err("no float8 overload");
    assert_eq!(err.sqlstate(), "42883");
    assert_eq!(
        err.to_string(),
        "function generate_series(integer, double precision) does not exist"
    );
}

/// `CREATE TABLE IF NOT EXISTS` is a NO-OP on an existing table.
///
/// The flag has to reach the executor, which is the only place that knows
/// whether the table is there: a fixture that creates a table if it is missing
/// used to fail the second time a session ran it.
#[test]
fn create_table_carries_if_not_exists() {
    match plan_ok("CREATE TABLE t (id int)") {
        Statement::CreateTable(_, if_not_exists) => assert!(!if_not_exists),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("CREATE TABLE IF NOT EXISTS t (id int)") {
        Statement::CreateTable(_, if_not_exists) => assert!(if_not_exists),
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn end_of_day_time_is_accepted_and_rendered_verbatim() {
    // PostgreSQL accepts 24:00:00 as a valid `time` and renders it back
    // verbatim; only the all-zero forms qualify.
    for input in ["24:00", "24:00:00", "24:00:00.000000"] {
        assert_eq!(
            super::parse_time(input).expect("valid"),
            "24:00:00",
            "{input}"
        );
    }
    // Anything past the end-of-day instant is out of range.
    for input in ["24:00:01", "24:00:00.1", "24:01", "25:00"] {
        assert!(super::parse_time(input).is_err(), "{input}");
    }
}

#[test]
fn epoch_literal_parses_on_date() {
    assert_eq!(super::parse_date("epoch").expect("valid"), "1970-01-01");
    assert_eq!(super::parse_date("EPOCH").expect("valid"), "1970-01-01");
}

#[test]
fn date_pg_text_covers_bc_and_wide_years() {
    use chrono::NaiveDate;
    assert_eq!(
        super::render_date_pg(NaiveDate::from_ymd_opt(2000, 1, 6).unwrap()),
        "2000-01-06"
    );
    // Year 0 in the proleptic calendar is 1 BC in PostgreSQL's rendering.
    assert_eq!(
        super::render_date_pg(NaiveDate::from_ymd_opt(0, 12, 31).unwrap()),
        "0001-12-31 BC"
    );
    assert_eq!(
        super::render_date_pg(NaiveDate::from_ymd_opt(10000, 1, 1).unwrap()),
        "10000-01-01"
    );
}

#[test]
fn datetime_arith_type_maps_the_operand_combinations() {
    let t = |op, l, r| super::datetime_arith_type(op, l, r);
    assert_eq!(t("+", "date", "int4"), Some("date"));
    assert_eq!(t("+", "int4", "date"), Some("date"));
    assert_eq!(t("-", "date", "date"), Some("int4"));
    assert_eq!(t("+", "timestamp", "interval"), Some("timestamp"));
    assert_eq!(t("+", "timestamptz", "interval"), Some("timestamptz"));
    assert_eq!(t("+", "interval", "interval"), Some("interval"));
    assert_eq!(t("*", "interval", "int4"), Some("interval"));
    assert_eq!(t("*", "numeric", "interval"), Some("interval"));
    assert_eq!(
        t("+", "timestamp with time zone", "interval"),
        Some("timestamptz")
    );
    // Not a datetime combination.
    assert_eq!(t("+", "int4", "int4"), None);
    assert_eq!(t("/", "int4", "interval"), None);
}

// -- constraints: names and shapes measured on PostgreSQL 16 (2026-09-09) --

#[test]
fn create_table_records_not_null_check_and_foreign_key_constraints() {
    let sql = "CREATE TEMP TABLE nm_t (a int not null, b serial, c int check (c > 0), \
               x int, check (a < b), check (x > 0), check (b*2 > a + c), \
               z int references t on delete cascade, \
               w int constraint fkw references t (id) deferrable initially deferred, \
               check (true))";
    let Statement::CreateTable(def, _) = plan_ok(sql) else {
        panic!("not a CREATE TABLE");
    };
    assert!(def.temp);
    assert!(!def.column("a").unwrap().nullable);
    assert!(!def.column("b").unwrap().nullable, "serial is NOT NULL");
    assert!(def.column("c").unwrap().nullable);
    let names: Vec<(&str, &str)> = def
        .check_constraints
        .iter()
        .map(|c| (c.name.as_str(), c.expression.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("nm_t_c_check", "(c > 0)"),
            ("nm_t_check", "(a < b)"),
            ("nm_t_check1", "((b * 2) > (a + c))"),
            ("nm_t_check2", "true"),
            ("nm_t_x_check", "(x > 0)"),
        ]
    );
    let fks: Vec<_> = def
        .foreign_keys
        .iter()
        .map(|f| {
            (
                f.name.as_str(),
                f.columns.clone(),
                f.ref_table.as_str(),
                f.ref_columns.clone(),
                f.on_delete.as_deref(),
                f.deferrable,
                f.initially_deferred,
            )
        })
        .collect();
    assert_eq!(
        fks,
        vec![
            (
                "nm_t_z_fkey",
                vec!["z".to_string()],
                "t",
                vec![],
                Some("CASCADE"),
                false,
                false
            ),
            (
                "fkw",
                vec!["w".to_string()],
                "t",
                vec!["id".to_string()],
                None,
                true,
                true
            ),
        ]
    );
}

#[test]
fn create_table_self_referencing_foreign_key_resolves_to_the_pk() {
    let Statement::CreateTable(def, _) = plan_ok(
        "create table selfref (x serial primary key, y int references selfref (x) \
         deferrable initially deferred)",
    ) else {
        panic!("not a CREATE TABLE");
    };
    let fk = &def.foreign_keys[0];
    assert_eq!(fk.name, "selfref_y_fkey");
    assert_eq!(fk.ref_columns, vec!["x".to_string()]);
    assert!(fk.deferrable && fk.initially_deferred);
    let err = plan(
        "create table s2 (x int primary key, u int, y int references s2 (u))",
        &lookup,
    )
    .unwrap_err();
    assert_eq!(err.sqlstate(), "42830");
    assert_eq!(
        err.to_string(),
        "there is no unique constraint matching given keys for referenced table \"s2\""
    );
}

#[test]
fn create_table_check_naming_a_missing_column_is_42703_at_create() {
    let err = plan("create table bad (a int, check (nope > 0))", &lookup).unwrap_err();
    assert_eq!(err.sqlstate(), "42703");
}

#[test]
fn check_expression_evaluates_false_true_and_null() {
    let def = t();
    let expr = plan_check_expression("(n > 0)", &def).unwrap();
    let row = |n: Bson| {
        let mut d = Document::new();
        d.insert("_id", 1);
        d.insert("n", n);
        d
    };
    assert_eq!(
        apply_row_expr(&expr, &row(Bson::Int32(1))).unwrap(),
        Bson::Boolean(true)
    );
    assert_eq!(
        apply_row_expr(&expr, &row(Bson::Int32(0))).unwrap(),
        Bson::Boolean(false)
    );
    assert_eq!(apply_row_expr(&expr, &row(Bson::Null)).unwrap(), Bson::Null);
}

#[test]
fn insert_with_untyped_parameters_takes_the_column_types() {
    // libpq's `PQprepare` with `nParams = 0` declares nothing; the values
    // still bind. Failed "there is no parameter $1" (2026-09-09).
    let stmt = plan_with_session_types(
        "insert into t values ($1, $2, $3)",
        &lookup,
        &[Bson::Int64(1), Bson::String("a".into()), Bson::Int64(2)],
        &[None, None, None],
        &TimeZoneSetting::default(),
    )
    .expect("should plan");
    match stmt {
        Statement::Insert(i) => assert_eq!(i.rows[0].get("_id"), Some(&Bson::Int32(1))),
        other => panic!("wrong statement: {other:?}"),
    }
}

#[test]
fn max_param_number_sees_the_values_of_an_insert() {
    assert_eq!(max_param_number("insert into t values ($1, $2)"), 2);
    assert_eq!(
        max_param_number("insert into t values ($1, $2) returning id"),
        2
    );
    assert_eq!(max_param_number("insert into t (id) select $3"), 3);
    assert_eq!(max_param_number("update t set n = $2 where id = $1"), 2);
    assert_eq!(max_param_number("select '$9' -- $8"), 0);
}

#[test]
fn create_and_drop_database_plan() {
    match plan_ok("CREATE DATABASE mydb WITH OWNER = joe") {
        Statement::CreateDatabase { name } => assert_eq!(name, "mydb"),
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("DROP DATABASE IF EXISTS mydb") {
        Statement::DropDatabase { name, if_exists } => {
            assert_eq!(name, "mydb");
            assert!(if_exists);
        }
        other => panic!("wrong statement: {other:?}"),
    }
    match plan_ok("SELECT current_catalog AS c") {
        Statement::SelectConstant(sc) => assert_eq!(
            sc.columns[0],
            (
                "c".to_string(),
                ConstCol::CurrentDatabase,
                "name".to_string(),
                -1
            )
        ),
        other => panic!("wrong statement: {other:?}"),
    }
}
