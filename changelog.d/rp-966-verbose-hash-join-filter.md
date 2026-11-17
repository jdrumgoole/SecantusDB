### EXPLAIN VERBOSE prints a hash join's cross-relation Join Filter

A hash join whose WHERE compares a column of each relation now prints under
`EXPLAIN VERBOSE` as PostgreSQL 15.19 does: every node's `Output`, the
comparison as the join's `Join Filter`, and the columns it reads in the
Hash's and the inner scan's `Output`. Before, such a plan printed without
any of its VERBOSE lists.

#### Fixed

- `EXPLAIN VERBOSE` of a two-relation hash join with a WHERE conjunct
  reading both relations prints every node's `Output` (RP-966, in part).

Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a hash join whose where reads both relations])
