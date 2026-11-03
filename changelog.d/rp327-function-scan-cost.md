### Rust PG server: EXPLAIN prices a `generate_series` Function Scan

`EXPLAIN select * from generate_series(1, 10)` now prints PostgreSQL 15's
estimate, `Function Scan on generate_series  (cost=0.00..0.10 rows=10
width=4)`, where it printed zeros.

#### Fixed

- A Function Scan of `generate_series` read whole is priced as
  `cost_functionscan` prices it: the support function's row count for its
  constant arguments (an empty series is one row), the call's
  `cpu_operator_cost` at startup, `cpu_tuple_cost` a row, and four or eight
  bytes a column for `int4` or `int8`.

Fixes RP-327 (tests/backlog/test_rp_explain_costs.py::test_plan[function scan])
