### EXPLAIN names a join chain's Hash Cond from its equivalence class

When the ONs of an inner join chain share a column, `secantusd-pg` now
prints each hash join's condition the way PostgreSQL derives it from their
equivalence class. A LEFT JOIN that an inner ON above it reduces to an
inner join counts too. Before, the server printed each join's own ON.

#### Fixed

- `explain (costs off) select e.b, k.b from e left join k on e.a = k.a
  join e e2 on e2.a = k.a`, and the same chain with an inner join, printed
  the upper join's `Hash Cond: (k.a = e2.a)`. PostgreSQL 15 prints
  `(e.a = e2.a)`. The class's members now go in the order
  `process_equivalence` adds them: each ON's two columns as written, bottom
  up, with classes merged. Each hash join takes the first member on each of
  its sides, outer side first (`generate_join_implied_equalities_normal`).
  This covers chains of any length drawn as written, under VERBOSE,
  ANALYZE, ORDER BY or a WHERE that fixes no member. Plans with a nested
  loop or merge join, ONs of more than one equality, and a WHERE equality
  or constant on a member keep their written conditions.
- Fixes RP-1061 (`tests/backlog/test_rp_explain_costs.py::test_plan[a left join reduced to inner joins the chain on its class]`)
  and RP-1057 (`tests/backlog/test_rp_explain_costs.py::test_plan[a three-way hash join names the class's first member]`).
