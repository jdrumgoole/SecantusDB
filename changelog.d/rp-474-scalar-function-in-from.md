### Rust PG server: a scalar function in FROM is a row source in every shape

A user function returning a plain scalar, called in FROM, was answered only
by the shapes with a path of their own (`select * from f()`, an alias, a
join). Under an aggregate, with a WHERE, or in `ROWS FROM` beside another
function it was refused with 0A000, where PostgreSQL's Function Scan yields
its one result as one row.

#### Fixed

- `select count(*) from f()`, `select * from f() as t(x) where x = 1`,
  `select * from f() t where t = 1`, `select f + 1 from f()` and `select *
  from rows from (f(), g())` over a scalar SQL function now answer
  PostgreSQL 15.19's rows instead of 0A000, and the function still runs
  once per statement.
  Fixes RP-474 (tests/backlog/test_rp_from_function_scan.py::test_scalar_function_in_from)
- Filed RP-476, found while working it: a function in FROM runs twice
  under an aggregate whose WHERE has a conjunct that reads no column
  (`count(*) from f() where 1 = 1`).
