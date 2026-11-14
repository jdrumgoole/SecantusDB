### Rust PG server: a function named in another database is a cross-database reference

A call with a three-part name, such as `select a.b.c()`, was looked up by
its last part alone and answered 42883 `function c() does not exist`.
PostgreSQL splits the name before it looks for any function
(`DeconstructQualifiedName`): a first part other than the session's
database is 0A000 `cross-database references are not implemented: a.b.c`
at the call, and four parts or more are 42601. The server now answers the
same, in the same order as PostgreSQL's other analysis errors.

#### Fixed

- A three-part call name whose first part is not the session's database
  is 0A000 `cross-database references are not implemented: <name>` at the
  call, the name printed as `NameListToString` prints it (unquoted, case
  kept: `"POSTGRES".pg_catalog.abs(-1)`). A built-in's name is refused too.
- A call name of four parts or more is 42601 `improper qualified name (too
  many dotted names): <name>`.
- In the session's own database the schema is looked up as for two parts:
  `postgres.nosuch.f()` is 3F000 `schema "nosuch" does not exist`.
- The statement's call and type-schema checks now reach a call's `FILTER`,
  its `ORDER BY` (a `WITHIN GROUP`'s too), its `OVER` and a named `WINDOW`,
  which pg_query's node walk skips. A refused name there
  (`count(*) filter (where nosuch.f())`) was 42883 before, and a type in a
  missing schema there 42704; both now answer as PostgreSQL does.
- A refused call name inside another call's arguments (`abs(a.b.c())`,
  `abs(nosuch.f())`) is that refusal: the arguments are analysed before
  the outer function is looked up. Before, the outer call's missing
  overload (`function abs(text) does not exist`) was the answer.

Fixes RP-880 (tests/backlog/test_rp_qualified_calls.py::test_three_part_function_name,
crates/secantus-pgplan/src/tests.rs::a_call_name_is_deconstructed_before_its_schema_is_looked_up).
Filed RP-950: a function found nowhere inside another call's arguments
(`abs(nosuch1())`) is still reported as the outer call's missing overload.
