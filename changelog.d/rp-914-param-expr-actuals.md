### EXPLAIN ANALYZE counts an index scan parameterized by an expression

A nested loop into an index whose condition compares an expression of the
outer row, `e.a + 1 < k.a`, now prints its scans' actual rows and loops,
and a Memoize over it prints its hits, misses and memory, as PostgreSQL
15.19 does. Before, only a condition on the outer row's bare column was
counted.

#### Fixed

- `analyze_param_nest_loop` accepts an outer operand `(alias.col ± n)`.
  Fixes RP-914 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a nested loop into an index on an expression of the outer side], tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a memoize keyed by an expression of the outer side])
