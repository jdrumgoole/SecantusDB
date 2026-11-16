### A Sort above an InitPlan's scan is priced

`EXPLAIN` of a single-table query that orders the rows a scan filtered by an uncorrelated scalar subquery now prices the Sort as PostgreSQL 15 does: the scan at its own cost, the Sort by `cost_sort` over it, and the InitPlan's cost on the Sort alone. The Sort Key is qualified by its table, as PostgreSQL prints it once the InitPlan has lengthened the range table.

#### Fixed

- `explain select a from e where b = (select 'v3'::text) order by a` printed `Sort (cost=0.00..0.00 rows=0 width=0)` and `Sort Key: a`; it is `Sort (cost=20.83..21.08 rows=100 width=4)`, `Sort Key: e.a`.

Fixes RP-440, part (tests/backlog/test_rp_explain_costs.py::test_plan[a sort above an initplan's scan is priced])
