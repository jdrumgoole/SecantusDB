### Rust PG server: EXPLAIN prices a Sort

`EXPLAIN select * from e order by b` printed `Sort  (cost=0.00..0.00 rows=0
width=0)` over a priced Seq Scan. The Sort now carries PostgreSQL 15's
`cost_sort` / `cost_tuplesort` estimate: the whole input read before the
first row, `2 * cpu_operator_cost` per comparison over `N log2 N`, and the
merge passes' page accesses once the rows outgrow the session's `work_mem`.

#### Fixed

- A Sort straight over one unindexed table's sequential scan prints
  PostgreSQL's estimate, and its Seq Scan prints its own; `SET work_mem`
  moves it between the in-memory and external-merge costs as it does on
  PostgreSQL 15.19. A filtered scan beneath the Sort is priced the same
  way, which was RP-332's case; a Seq Scan under any other node is
  asserted by that node's own item.

Fixes RP-318 (tests/backlog/test_rp_explain_costs.py::test_plan[sort],
tests/backlog/test_rp_explain_costs.py::test_plan[a sort past work_mem]),
RP-332 (tests/backlog/test_rp_explain_costs.py::test_plan[a seq scan beneath another node])
