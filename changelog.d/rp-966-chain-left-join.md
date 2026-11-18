### EXPLAIN VERBOSE prints a three-way join ending in a LEFT JOIN

A left-deep chain of hash joins whose topmost join is a LEFT JOIN now prints every node's `Output` under VERBOSE, as PostgreSQL 15.19 does. The outer join's ON forms no equivalence class, so a column it compares stays in a lower join's `Output` only while that ON still needs it.

#### Fixed

- `EXPLAIN (VERBOSE)` of a three-way hash-join chain ending in a LEFT JOIN printed the plain plan. Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a three-way join ending in a left join]).
