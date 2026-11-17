### An immutable call over constants is folded in a WHERE

`explain select * from e where abs(-3) < a` now prints `Filter: (3 < a)`
and prices the scan from `a`'s statistics (`rows=997`), as PostgreSQL
does: `eval_const_expressions` runs an immutable built-in whose
arguments are all constants while planning, and a comparison with the
constant on its left is estimated, and filtered, through the operator's
commutator. Before, the call printed unfolded and the scan had no
estimate at all.

#### Fixed

- A WHERE's call of an immutable built-in over non-NULL constants is
  folded to its value in EXPLAIN (integer and boolean results); a data
  exception the call raises is the plan's error, as in PostgreSQL.
- `const < column` (and `=`, `<>`, `<=`, `>`, `>=`) is lowered as
  `column > const`, so it is estimated and scanned as the column's clause.
  Fixes RP-1021 (tests/backlog/test_rp_explain_costs.py::test_plan[a call over constants in a where is folded])
