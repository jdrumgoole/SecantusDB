### Counting the rows an InitPlan filters is an Aggregate over the scan

`EXPLAIN select count(*) from e where b = (select 'v3'::text)` on the Rust
PostgreSQL server printed an unpriced `Subquery Scan on e` between the
Aggregate and the scan, which PostgreSQL does not have: it merges the
subquery the executor runs that WHERE as. The Aggregate now sits straight
over the scan and is priced as PostgreSQL 15 prices it, the InitPlan's cost
on the top node.

#### Fixed

- Rust PG server: counts over one table whose WHERE compares a column with
  an uncorrelated scalar subquery print no Subquery Scan and carry
  `cost_agg`'s estimate. Fixes RP-440 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over an initplan's scan is not a subquery scan]).
