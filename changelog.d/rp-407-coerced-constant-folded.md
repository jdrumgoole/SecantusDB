### EXPLAIN folds a constant coerced to its operator's type

PostgreSQL's planner folds the implicit coercion of a constant before EXPLAIN prints it: `a::numeric > 4` filters on `'4'::numeric`, and a constant made double precision prints in `float8out`'s form (`'1e-05'::double precision`). The Rust PostgreSQL server now prints the folded constant, alone or as a join's scan, and places an expression equal to such a constant after the scan's other quals, as its equivalence class is.

#### Fixed

- EXPLAIN prints a constant coerced to an integer, numeric or double precision operator folded, and orders an expression's equality with a constant last.
  Fixes RP-407 (tests/backlog/test_rp_explain_costs.py::test_plan[a scan filter's coerced constant is folded])
