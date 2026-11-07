### Rust PG server: a function called in FROM runs once

A user function called in FROM ran its body more than once, so every write
in it happened more than once: `select * from f()` over a body that inserts
a row inserted two, a FROM subquery or CTE over it four, `WITH ORDINALITY`
twelve, and `EXPLAIN` without ANALYZE, which runs nothing on PostgreSQL,
one. The statement's operator-type check and the whole-row rewrite of a
FROM subquery each planned the function only to learn its columns, and
that planning called it. They now plan it with calls switched off, as a
Describe does, so the statement's own plan is the one call, as on
PostgreSQL 15.19, over the simple and the extended protocol alike.

#### Fixed

- A user function in FROM (scalar or SETOF, directly, in a FROM subquery,
  in a CTE, `WITH ORDINALITY`, or feeding `INSERT ... SELECT`) runs once
  per statement, and `EXPLAIN` without ANALYZE runs it not at all.
  Fixes RP-472 (tests/backlog/test_rp_from_function_runs_once.py::test_from_function_runs_once,
  tests/backlog/test_rp_from_function_runs_once.py::test_explain_runs_nothing)
- Filed, found while working it: RP-473, a scan sees the rows a function
  the same statement ran wrote
  (tests/backlog/test_rp_from_function_scan.py::test_scan_does_not_see_the_functions_write);
  RP-474, a scalar user function in FROM is refused under an aggregate,
  an alias column list with a WHERE, and ROWS FROM
  (tests/backlog/test_rp_from_function_scan.py::test_scalar_function_in_from);
  RP-475, EXPLAIN of a user function in FROM prints no Function Scan
  (tests/backlog/test_rp_from_function_scan.py::test_function_scan_plan).
