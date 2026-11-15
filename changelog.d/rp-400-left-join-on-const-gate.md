### A LEFT join's contradictory ON constants gate its nullable side

When a LEFT join's ON equates a column of the nullable side to two different constants, PostgreSQL's equivalence class keeps the first as that side's filter and turns the rest into a `false` pseudoconstant on a `Result` over its scan. EXPLAIN on the Rust PostgreSQL server now prints the same gated plan instead of a scan filtering on both constants.

#### Fixed

- EXPLAIN of `select * from s left join e on s.a < e.a and e.b = 'v1' and e.b = 'v2'` prints a `Result` with `One-Time Filter: false` over `e`'s scan, filtered by `b = 'v1'` only.
  Fixes RP-400 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join's on equating its nullable column to two constants])
