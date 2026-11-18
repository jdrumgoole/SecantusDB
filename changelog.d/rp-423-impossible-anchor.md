### A regex that cannot match is estimated

A regular expression whose every path meets a `^` after a character, such
as `^w(?:^0)1`, can never match. PostgreSQL's regex compiler marks it
impossible and the planner finds no prefix, estimating the scan at one
row; the Rust PostgreSQL server now does the same instead of printing
zero costs.

#### Fixed

- `EXPLAIN` of a scan filtered by an unsatisfiable regex prints
  PostgreSQL 15.19's estimate (`rows=1`).

Fixes RP-423, in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a regex match with an anchor after a character])
