### Rust PG server: EXPLAIN prices a Nested Loop of two Seq Scans

`EXPLAIN` of a join of two tables with nothing to hash or merge on, such as
`select * from e, e e2 where e.a < e2.a`, now prints PostgreSQL 15.19's plan.
It has the `Nested Loop`'s cost, rows and width, the `Join Filter` of a
comma-FROM's WHERE, and the `Materialize` over the inner scan. The outer side
is the one PostgreSQL's cost model picks, so `FROM zs, e` puts the larger
table outer. Before this, every number was zero and the Materialize and Join
Filter lines were missing.

#### Fixed

- A Nested Loop of two sequential scans joined by `<`, `<=`, `>` or `>=`
  conjuncts, or by none, over tables with no index, prints `cost_nestloop`'s
  estimate. Its inner side is read through a `Materialize`
  (`cost_material`) when that is cheaper. Each scan's width is the columns
  the join reads from it. Other nested loops still print zeros (RP-354).
  Fixes RP-329 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop])
