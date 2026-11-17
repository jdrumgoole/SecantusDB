### An equivalence class's constant is the inner index scan's Index Cond

When a join's WHERE fixes one join key to a constant, PostgreSQL gives the
other table its own copy of that equality, and an index on that column takes
it as an index qual. The Rust PostgreSQL server's EXPLAIN now prints it as the
inner index scan's `Index Cond: (a = 5)` rather than a `Filter`, as
PostgreSQL 15.19 does for `select * from e, k where e.a = k.a and e.a = 5`.

#### Fixed

- EXPLAIN places a generated equivalence-class constant on an index scan's
  key column in its Index Cond.
  Fixes RP-977 (tests/backlog/test_rp_explain_costs.py::test_plan[an equivalence class's generated constant is the inner index's cond])
