### EXPLAIN VERBOSE prints a nested loop's output lists

`EXPLAIN VERBOSE` of a two-table join that PostgreSQL runs as a nested loop
over a parameterised index scan now prints what PostgreSQL 15.19 prints:
each node's `Output` list, both relations schema-qualified, and the inner
index scan's `Index Cond` with its own column prefixed.

#### Fixed

- Rust PG server: `EXPLAIN VERBOSE` of a nested loop with an inner index
  scan prints every node's `Output` and the prefixed `Index Cond`.
  Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a nested loop's nodes print their output under verbose])
