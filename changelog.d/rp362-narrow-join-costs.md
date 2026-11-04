### Rust PG server: EXPLAIN prices LEFT, filtered and cross joins that select some columns

`EXPLAIN` of a two-table join whose select list names columns now prints
PostgreSQL 15.19's costs, rows and widths in three more shapes. Before this
they printed zeros:

- a LEFT join, which becomes a `Hash Right Join` where PostgreSQL hashes the
  preserved side;
- a WHERE on either side, which filters that side's scan;
- a cross join, which is a `Nested Loop` over a `Materialize`.

#### Fixed

- A narrow-path LEFT join is priced as `cost_hashjoin` with at least the
  preserved side's rows (`calc_joinrel_size_estimate`). When PostgreSQL
  hashes the preserved side, it prints `Hash Right Join` with that side
  outer.
- A WHERE on one side is that side's scan filter. The scan's rows and the
  hashed side's bucket statistics follow from it.
- A cross join over tables with no index is `cheapest_nest_loop`'s plan,
  as RP-329 prices a comma FROM.
- A WHERE that fixes a join key or reduces a LEFT join, and a WHERE over an
  indexed table, still print zeros (RP-363).
  Fixes RP-362 (tests/backlog/test_rp_explain_costs.py::test_plan[a projected LEFT join hashed as a right join],
  `[a projected join under a WHERE]`, `[a projected cross join]`)
