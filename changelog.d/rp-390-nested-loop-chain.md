### EXPLAIN ANALYZE counts every join of a nested-loop chain

A chain of three or more tables joined by plain Nested Loops now prints
actual rows, loops and `Rows Removed by Join Filter` at every level, as
PostgreSQL 15.19 does; before, only the top join was counted and the joins
below it printed bare. Each lower join's rows are counted by running its
Join Filter over its pairs, and the join above reads them as its outer
side.

#### Fixed

- `explain (analyze) select * from z join z z2 on z.a < z2.a join z z3 on
  z2.a < z3.a` prints the lower Nested Loop's `(actual rows=190 loops=1)`
  and its Materialize's `loops=20`.

Fixes RP-390, part (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a nested loop of three relations]; crates/secantus-pgplan/src/tests.rs::a_relations_expression_reads_each_aliass_row)
