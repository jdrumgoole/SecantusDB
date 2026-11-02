### Rust PG server: EXPLAIN prints a Result's One-Time Filter

A select with no FROM whose WHERE folds to `false` or NULL now shows
PostgreSQL 15's gating qual under its `Result`: `One-Time Filter: false`
or `One-Time Filter: NULL::boolean`; one folded to `true` prints none, as
there.

#### Fixed

- `explain select 1 where false` prints `One-Time Filter: false`.
  Fixes RP-341 (tests/backlog/test_rp_explain_costs.py::test_plan[a result's one-time filter]).

#### Changed

- Files RP-342: a WHERE that calls a function (not folded by PostgreSQL)
  still prints no One-Time Filter.
