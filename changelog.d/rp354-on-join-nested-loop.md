### Rust PG server: EXPLAIN prices a JOIN ... ON's nested loop, inner or LEFT

`EXPLAIN` of a two-table nested loop written as `JOIN ... ON` with `<`,
`<=`, `>`, `>=` or `<>` conditions now prints PostgreSQL 15.19's estimate
instead of zeros, for a LEFT join as well as an inner one, and over a table
with an index when the condition could not be that index's Index Cond.

#### Fixed

- A `JOIN ... ON` whose conditions are inequalities or `<>` between the two
  tables is priced as `cost_nestloop`, the same as the comma join's WHERE.
- A LEFT join's nested loop keeps its nullable side inner even where the
  other order is cheaper, and never estimates fewer rows than its outer
  side (`calc_joinrel_size_estimate`).
- A table with a primary key joined by `<>` is priced: its plain Seq Scan
  is the inner PostgreSQL weighs, and its key column is read from where the
  table stores it.

Fixes part of RP-354 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop on an ON's inequality], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join, the smaller side outer], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join into a primary key's table]); the parameterized nested loop into an index and an ON conjunct over one side stay open.
