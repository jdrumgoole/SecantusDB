### EXPLAIN VERBOSE prints a hash join's targetlists

`explain verbose select * from e join k on e.a = k.a` now prints as
PostgreSQL does: `Output: e.a, e.b, k.a, k.b` and `Inner Unique: true` on
the Hash Join, each scan named schema-qualified with its own `Output`, the
outer scan returning its whole row and the Hash and the scan under it only
the columns the join needs. The structured formats print a VERBOSE
`Output` as a list, ahead of a join's `Inner Unique`. Nested loops, merge
joins and joins with a WHERE still print as without VERBOSE.

#### Fixed

- EXPLAIN VERBOSE of a two-relation hash join on column equalities prints
  every node's `Output` and the join's `Inner Unique`.
  Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a join's nodes print their output under verbose])
