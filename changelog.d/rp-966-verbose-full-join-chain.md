### EXPLAIN VERBOSE prints a join chain topped by a FULL JOIN

`EXPLAIN VERBOSE` of a chain of hash joins whose last join is a FULL JOIN
now prints every node's `Output` as PostgreSQL 15.19 does: the `Hash Full
Join` hashes its right relation alone, and each join below carries only the
columns still needed above it.

#### Fixed

- Rust PG server: `EXPLAIN VERBOSE` of `... join k on ... full join e e2 on
  ...` prints each node's `Output` instead of the plain plan. Fixes RP-966
  in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a chain ending in a full join]).
- Filed RP-1062 (an outer join's two-clause `Hash Cond` order) and RP-1063
  (a RIGHT JOIN atop a chain).
