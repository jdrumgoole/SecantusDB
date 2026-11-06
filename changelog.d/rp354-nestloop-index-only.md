### Rust PG server: EXPLAIN prices a nested loop into an index-only scan

A nested loop whose inner side is read through a B-tree holding every
column it reads -- `select k.a from e, k where e.a < k.a` -- now prints
PostgreSQL 15.19's `Index Only Scan using k_pkey on k` with its estimate,
its heap fetches cut by the table's all-visible fraction as `cost_index`
cuts them, instead of a plan of zeros. With `enable_indexonlyscan` off it
is the plain Index Scan, as in PostgreSQL.

#### Fixed

- EXPLAIN of a nested loop whose parameterized inner index covers the
  columns read.

Fixes RP-354, part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into an index-only scan])
