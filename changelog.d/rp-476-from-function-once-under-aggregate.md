### Rust PG server: a function in FROM runs once under an aggregate

A volatile function called in FROM under an aggregate ran twice when the
aggregate planner's first plan was refused and a rewrite planned the
statement again: a WHERE conjunct that reads no column (`count(*) from f()
where 1 = 1`), or a grouped expression over a WHERE (`x + 1, count(*) ...
where 1 = 1 group by x`). Each plan ran the function, so a body that writes
wrote twice. PostgreSQL's Function Scan runs it once. Now, when an aggregate
or GROUP BY reads a FROM item that runs a volatile call, that first attempt
is planned with calls switched off, and it is planned for real only when
its plan will stand.

#### Fixed

- `select count(*) from f() where 1 = 1`, `select max(x) from f() as t(x)
  where x > 0 and 1 = 1`, `select x, count(*) ... where 1 = 1 group by x`,
  `select x + 1, count(*) ... where 1 = 1 group by x` (and its CTE form),
  and the same under the extended protocol with `$1 = 1`, now run a
  writing SQL function once, as PostgreSQL 15.19 does.
  Fixes RP-476 (tests/backlog/test_rp_from_function_scan.py::test_aggregate_over_where_runs_the_function_once)
