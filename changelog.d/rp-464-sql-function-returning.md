### Rust PG server: a SQL function ending in a write's RETURNING answers its rows

A `LANGUAGE sql` function whose last statement was an INSERT, UPDATE or
DELETE with a RETURNING list answered NULL: the body ran the write for its
effect and dropped the rows it returned. `insert into fs values (p, 'i')
returning p * 2` now answers 8 for 4, as on PostgreSQL 15.19, and a
set-returning function's result is the whole RETURNING list.

#### Fixed

- A SQL function's write with a RETURNING list yields rows, and as the
  body's last statement those rows are the function's result. A scalar
  function's write runs to completion though only its first row is the
  value; a write that returns no row is NULL.
  Fixes RP-464 (tests/backlog/test_rp_function_bodies.py::test_returning_statement_is_the_result,
  tests/backlog/test_rp_function_bodies.py::test_returning_shapes)
- Filed RP-472, found while working it: a function called in FROM runs its
  body twice, so its writes happen twice
  (tests/backlog/test_rp_from_function_runs_once.py::test_from_function_runs_once).
