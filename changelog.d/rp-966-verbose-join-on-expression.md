### EXPLAIN VERBOSE prints a join on an expression

A join of two relations whose ON compares an expression of one with the
other's column, `e join k on e.a = k.a + 1`, now prints every node's
`Output` under `EXPLAIN VERBOSE` as PostgreSQL 15.19 does, for a hash join
and for a nested loop over a Materialize: the columns the expression reads
are carried up from its scan to the join.

#### Fixed

- EXPLAIN VERBOSE of a two-relation join whose ON compares expressions prints
  each node's `Output` instead of the plain plan (RP-966, in part; RP-1058 filed).
  Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a join on an expression], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a materialized loop on an expression])
