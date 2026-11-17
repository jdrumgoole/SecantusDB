### EXPLAIN pulls a simple FROM-subquery up into its parent

PostgreSQL merges a simple FROM-subquery into the query around it before it
plans, so `explain select * from (select * from e where a > 5) s` is one
`Seq Scan on e` with `Filter: (a > 5)`. The Rust PostgreSQL server now does
the same for a subquery over one table with at most a WHERE, instead of
printing a `Subquery Scan` over a scan that showed no filter.

#### Fixed

- EXPLAIN of a plain FROM-subquery prints the pulled-up scan and its
  filter, as `pull_up_simple_subquery` plans it.
  Fixes RP-422 (tests/backlog/test_rp_explain_costs.py::test_plan[a plain from-subquery is pulled up])
