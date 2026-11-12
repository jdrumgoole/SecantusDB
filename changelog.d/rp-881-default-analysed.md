### Rust PG server: a DEFAULT is analysed when it is declared

A column's DEFAULT was stored without being analysed, so
`create table t (a int default nosuch_fn())` succeeded and the error only
appeared at the first INSERT that used the default. A default that
PostgreSQL refuses outright was accepted the same way: one that reads a
column, holds a subquery, or calls an aggregate, a window function or a
set-returning function. PostgreSQL analyses a default where it is declared
(`cookDefault`) and refuses each of these there. This server now does the
same, with PostgreSQL's errors, in PostgreSQL's order.

#### Fixed

- CREATE TABLE, ALTER TABLE ... ADD COLUMN, ALTER TABLE ... ALTER COLUMN
  SET DEFAULT, a partition's column options, CREATE DOMAIN and ALTER
  DOMAIN ... SET DEFAULT now analyse the default as PostgreSQL 15.19 does.
  Each of these is refused:
  - a column reference, bound or not: 0A000 `cannot use column reference
    in DEFAULT expression`;
  - a subquery, EXISTS or ARRAY(...): 0A000 `cannot use subquery in
    DEFAULT expression`;
  - a function found nowhere: 42883. A schema that does not exist is
    3F000;
  - an aggregate: 42803. A window function is 42P20, and a set-returning
    function is 0A000. Each message is `... are not allowed in DEFAULT
    expressions`;
  - OVER on a plain function, or a window function with no OVER: 42809.
- The first failing token is the one reported. A call's arguments are
  analysed before the call, and an operator's left operand before its
  right. CREATE TABLE analyses its defaults after the column types and
  names and before its CHECK constraints, as `AddRelationNewConstraints`
  does. ALTER TABLE and the domain statements report no position, as on
  PostgreSQL.
- `CREATE TABLE IF NOT EXISTS` over a table that exists analyses no
  default.
- Fixes RP-881
  (tests/backlog/test_rp_qualified_calls.py::test_create_table_default_is_analysed,
  tests/backlog/test_rp_qualified_calls.py::test_default_is_analysed_as_cook_default_does).
  Planner regression test:
  `crates/secantus-pgplan/src/tests.rs::create_table_analyses_each_default`.
- Filed while working it, each with a gap test in
  `tests/backlog/test_rp_found_rp881.py`:
  - RP-886: a constant DEFAULT is evaluated at CREATE TABLE
    (`default 1/0` is 22012 there).
  - RP-887: CREATE TABLE IF NOT EXISTS over a table that exists still
    checks its columns' types, names and literal defaults.
  - RP-888: `default nextval('nosuch_seq')` passes CREATE TABLE.
