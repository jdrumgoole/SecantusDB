### A COALESCE's constant arguments are folded

PostgreSQL's planner simplifies a COALESCE before it plans a scan: NULL constant arguments are dropped, the list ends at the first non-NULL constant, and a COALESCE whose first argument is a constant is that constant. `coalesce(null, a) = 1` filters on `COALESCE(a) = 1`, and `coalesce(1, a) = 1` folds to true, a scan with no filter at all. The Rust PostgreSQL server's EXPLAIN now plans it the same way.

#### Fixed

- EXPLAIN folds a COALESCE's constant arguments as `eval_const_expressions` does, and a comparison of two literals that folding leaves.
  Fixes RP-421 (tests/backlog/test_rp_explain_costs.py::test_plan[a coalesce drops its null arguments], tests/backlog/test_rp_explain_costs.py::test_plan[a coalesce of a constant first folds])
