### A LIMIT over an ORDER BY beside an InitPlan is priced

The PostgreSQL server's EXPLAIN now prices a LIMIT above a Sort of a scan
whose WHERE compares a column with an uncorrelated scalar subquery. The Sort
is costed as PostgreSQL's bounded top-N heap sort, which keeps only OFFSET +
LIMIT rows, and the Limit and InitPlan costs sit on top, matching
PostgreSQL 15.19 to the cent.

#### Fixed

- `explain select a from e where b = (select 'v3'::text) order by a limit 5`
  printed a zero-cost Limit; it now prints PostgreSQL's
  `Limit (cost=19.17..19.18 rows=5)` over `Sort (cost=19.16..19.41)`.
  Partly fixes RP-440 (tests/backlog/test_rp_explain_costs.py::test_plan[a limit above a sort above an initplan's scan is priced]).
