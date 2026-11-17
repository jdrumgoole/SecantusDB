### EXPLAIN VERBOSE prefixes a nested loop's inner filter on the ON

A nested loop into a parameterised index scan whose ON carries a second
comparison, such as `e left join k on e.a < k.a and e.b <> k.b`, now prints
under `EXPLAIN VERBOSE` as PostgreSQL 15.19 does: every node's `Output` list,
and the inner scan's `Filter: (e.b <> k.b)` with both columns prefixed.

#### Fixed

- `EXPLAIN VERBOSE` of a nested loop whose inner index scan filters on an ON
  comparison reading both relations printed the plain plan.
  Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a nested loop whose inner filter reads both relations])
