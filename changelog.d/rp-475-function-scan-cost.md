### EXPLAIN prices a user function in FROM

A user function called in FROM now prints the Function Scan PostgreSQL
prices with `cost_functionscan`: its `procost` times `cpu_operator_cost` at
startup, then `cpu_tuple_cost` for each of its `prorows` rows, so a SETOF
SQL function shows `(cost=0.25..10.25 rows=1000 width=4)` and a scalar one
`(cost=0.25..0.26 rows=1 width=4)`, where every cost used to be zero. A
scalar SQL function whose body is one constant SELECT is inlined and prints
`Result`, as PostgreSQL's planner folds it.

#### Fixed

- Rust PostgreSQL server: EXPLAIN of a user function in FROM is priced from
  its `pg_proc` row, and a constant scalar SQL function inlines to a
  `Result`. A WHERE over the call still prints no Filter (RP-475, trimmed).
  Fixes part of RP-475 (tests/backlog/test_rp_from_function_scan.py::test_function_scan_cost)
