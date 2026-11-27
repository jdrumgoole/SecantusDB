### A comma join of three tables takes its WHERE constant's equivalence class in EXPLAIN

The Rust PostgreSQL server's EXPLAIN of `FROM e, k, k k2 WHERE e.a = k.a
AND k2.a = e.a AND e.a = 3` now prints the plan PostgreSQL prints, and the
plan the same query written with explicit JOINs already got: Nested Loops,
each inner table read through its index with `Index Cond: (a = 3)`, and no
join clause left.

#### Fixed

- A comma join of three or more tables whose WHERE equalities put every
  table's key in one equivalence class, and whose WHERE fixes a member to a
  constant, printed Hash Joins over Seq Scans with each equality as a `Join
  Filter`. The WHERE's equalities now join the class as an ON's do
  (`deconstruct_jointree` makes them one qual list), and the constant
  replaces them (`generate_base_implied_equalities_const`).

Fixes RP-1105 (tests/backlog/test_rp_explain_costs.py::test_plan[a three-way comma join's class constant is each inner index's cond])
