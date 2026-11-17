### EXPLAIN VERBOSE prints a join whose WHERE compares an expression with the other relation

A two-relation join whose WHERE compares an expression of one relation with
a column of the other -- `length(e.b) < k.a` -- printed the plain plan under
`EXPLAIN VERBOSE`. It now prints every node's `Output`, the scans
schema-qualified and the conjunct prefixed, whether the planner pushes it
into a parameterised inner index scan's `Index Cond` or evaluates it as the
join's `Join Filter`, as PostgreSQL 15.19 does.

#### Fixed

- Rust PG server: `EXPLAIN VERBOSE` of a join whose WHERE compares an
  expression of one relation with the other's column prints its `Output`
  lists and prefixed conditions. Fixes RP-966 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a nested loop whose where compares an expression])
