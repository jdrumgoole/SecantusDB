### Rust PG server: EXPLAIN of a join whose WHERE fixes the join key

`EXPLAIN` of a two-table inner join whose WHERE sets a join key to a
constant, such as `select e.a from e join k on e.b = k.b where e.b = 'v1'`,
now prints PostgreSQL 15.19's plan. The constant joins the keys'
equivalence class, so both tables are filtered by it and no join clause is
left. The plan is a `Nested Loop` over the two filtered scans, the inner one
under a `Materialize`, priced as PostgreSQL prices it. Before this it printed
a `Hash Join` with a `Hash Cond`, all zeros, and only the side the WHERE
named carried the filter.

#### Fixed

- The narrow join path's inner join whose WHERE fixes a join key follows
  `generate_base_implied_equalities_const`. The other side's scan gets the
  same constant as a `Filter`, ANDed after its own quals in one line, and
  no `Join Filter` or `Hash Cond` is printed. `cheapest_nest_loop` over the
  two filtered scans picks the outer side and the `Materialize`.
- A LEFT join whose WHERE changes it still prints zeros over the wrong tree
  (RP-364).
  Fixes RP-363 (tests/backlog/test_rp_explain_costs.py::test_plan[a projected join whose WHERE fixes the key])
