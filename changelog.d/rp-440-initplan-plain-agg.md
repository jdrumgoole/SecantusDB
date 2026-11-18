### Any plain aggregate over an InitPlan's scan is priced

`EXPLAIN` of a plain aggregate such as `sum`, `avg`, `min` or `max` over one
table whose WHERE compares a column with an uncorrelated scalar subquery now
prints PostgreSQL's plan with its costs: the Aggregate straight over the Seq
Scan, the InitPlan on top. Only counts were priced before; every other
aggregate printed zero costs over a Subquery Scan.

#### Fixed

- An aggregate other than a count over an InitPlan's scan is priced by
  `cost_agg` (AGG_PLAIN), its Subquery Scan dropped.
  Fixes RP-440, part (tests/backlog/test_rp_explain_costs.py::test_plan[a sum above an initplan's scan is priced])
