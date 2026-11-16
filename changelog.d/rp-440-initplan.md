### EXPLAIN plans an uncorrelated scalar subquery as an InitPlan

A WHERE that compares a column with an uncorrelated scalar subquery, such
as `b = (select 'v3'::text)`, now explains as PostgreSQL plans it: the
subquery is an `InitPlan` run once before the scan, and the Filter reads
its output parameter `$0`. The scan is priced with the comparison to a
value unknown at plan time, and the InitPlan's cost is added to the scan's
startup and total. Several subqueries are numbered in order, and the
structured formats carry each one with `Parent Relationship: InitPlan` and
its `Subplan Name`.

#### Fixed

- Rust PG server: `explain select * from e where b = (select 'v3'::text)`
  printed the subquery inline in the Filter with every cost zero; it is
  now `Seq Scan on e (cost=0.01..17.51 rows=100 width=7)`, `Filter: (b =
  $0)`, over `InitPlan 1 (returns $0)`, as on PostgreSQL 15.19.
  Fixes RP-440, in part (tests/backlog/test_rp_explain_costs.py::test_plan[an uncorrelated subquery is an initplan])
