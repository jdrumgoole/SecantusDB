### EXPLAIN VERBOSE prints an inner index scan's filter over both relations

A nested loop into a parameterised index scan whose WHERE holds an
expression over both relations, `where e.a + k.a < 7`, evaluates it as that
scan's `Filter`. Under VERBOSE SecantusDB's PostgreSQL server fell back to
the plain plan for it, with no `Output` lists; it now prints the whole
verbose plan, the filter prefixed as `((e.a + k.a) < 7)`, as PostgreSQL 15
does.

#### Fixed

- Rust PG server: EXPLAIN VERBOSE of a nested loop whose inner index scan
  filters an expression of both relations prints every node's `Output` and
  the filter prefixed.
  Fixes RP-966, in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of an inner index scan filtering an expression of both])
