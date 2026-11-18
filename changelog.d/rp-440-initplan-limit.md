### A Limit over an InitPlan's scan is priced

A query over one table whose WHERE compares a column with an uncorrelated
scalar subquery, ending in LIMIT or OFFSET, now prints PostgreSQL's costs on
its Limit node in EXPLAIN: the scan's estimate cut by `adjust_limit_rows_costs`,
with the InitPlan's cost on the top node only, and a LIMIT 0 counted as one row.

#### Fixed

- EXPLAIN of a Limit straight over an InitPlan's scan printed zero costs.
  Part of RP-440 (tests/backlog/test_rp_explain_costs.py::test_plan[a limit above an initplan's scan is priced]).
