### RP-077's latency test times the subquery, and the cold catalog after DDL is filed

#### Fixed

- `tests/backlog/test_rp_catalog_latency.py::test_psql_describe_collation_subquery`
  (RP-077) timed the first query after its own `CREATE TABLE`. That run also
  pays for re-decoding every catalog collection after DDL, and on a slow CI
  runner it took 2.2 s against the 2.0 s budget (PR #2552, 2026-11-09). An
  A/B with and without the PR's change answered 0.578 s and 0.575 s locally
  (medians of 8), so nothing had regressed. The test now times the query's
  second run, which still evaluates the subquery for every column: about
  0.016 s on a debug build, against the 64 s RP-077 fixed.

#### Known issues

- RP-715: any DDL statement makes the next catalog query decode every
  catalog collection again (0.58 s on a debug build, growing to 1.79 s
  with 400 more tables), where PostgreSQL 15.19 invalidates only what
  changed and answers in 0.9 ms. Gap test:
  `tests/backlog/test_rp_catalog_latency.py::test_the_first_catalog_query_after_ddl_costs_what_the_next_does`.
