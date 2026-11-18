### Grouped counts above an InitPlan's scan are priced

On the Rust PostgreSQL server, `EXPLAIN` of a `GROUP BY` that counts rows
whose WHERE compares a column with an uncorrelated scalar subquery now
prints the `HashAggregate` straight over the scan, as PostgreSQL 15 plans
it, its Group Key qualified and its cost, rows and width estimated, where
before it printed an unpriced Subquery Scan beneath it.

#### Fixed

- Counts grouped by stored columns over an InitPlan's scan, on a table with
  no index, drop the executor's merged FROM subquery and are priced by
  `cost_agg` (AGG_HASHED).
  Trims RP-440 (tests/backlog/test_rp_explain_costs.py::test_plan[a group by above an initplan's scan is priced]).
