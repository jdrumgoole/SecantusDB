### Rust PG server: a call in a schema that does not exist is 3F000

A call qualified with a schema that does not exist, such as
`select nosuch_schema.f()`, answered 42883 `function f() does not exist`,
with the schema dropped and the position on the parenthesis. PostgreSQL
looks up the schema before any function (`LookupExplicitNamespace`) and
answers 3F000 `schema "nosuch_schema" does not exist` at the call.

#### Fixed

- A call in a missing schema is 3F000 at the call. This includes a
  built-in's name (`nosuch_schema.abs(-2)`, `nosuch_schema.count(*)`),
  which was found through any schema before. It covers the select list,
  WHERE, FROM, VALUES rows, EXPLAIN, `CALL` and the extended protocol.
  PostgreSQL's order is kept:
  - an error met earlier in analysis still stands, whether in the FROM
    clause, the call's arguments or an earlier target (42P01, 42703,
    another function written before it);
  - a later one does not (`select nosuch_schema.f(), realmissing()`,
    `select 1/0, nosuch_schema.f()`).
- The statement is planned without running anything, so a subquery in the
  call's arguments no longer draws a sequence value before the error.
- `pg_temp` is a schema only once the session has made a temporary
  object, as on PostgreSQL. The session now keeps its temp namespace after
  the last object in it is dropped (`myTempNamespace`). A temp table that a
  rolled-back transaction made leaves none behind. A qualified `%TYPE` or
  `%ROWTYPE` through `pg_temp` follows the same rule.
- Expressions in a `VALUES` list's rows are now walked by the statement's
  schema checks. pg_query's node walk skips those rows, so a qualified type
  there (`values (1::nosuch.t)`) is now checked too.
- Fixes RP-485
  (tests/backlog/test_rp_function_bodies.py::test_call_in_a_missing_schema,
  tests/backlog/test_rp_function_bodies.py::test_missing_schema_call_in_a_session).
  Planner regression test:
  `crates/secantus-pgplan/src/tests.rs::a_call_in_a_missing_schema_is_3f000`.
- Filed while working it, each with a gap test in
  `tests/backlog/test_rp_qualified_calls.py`:
  - RP-879: a missing function in a named schema loses the schema from
    its message.
  - RP-880: a three-part function name is not a cross-database 0A000.
  - RP-881: CREATE TABLE does not analyse a column's DEFAULT.
  - RP-882: a missing `pg_temp` relation is named `pg_temp_N`.
  - RP-883: the temp namespace does not follow a transaction that makes
    and drops a temp table.
