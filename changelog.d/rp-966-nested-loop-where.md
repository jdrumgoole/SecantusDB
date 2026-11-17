### EXPLAIN VERBOSE prints a filtered nested loop's output lists

A two-relation nested loop into a parameterised index scan now prints its
VERBOSE form when the query also has a WHERE whose conjuncts each read one
relation: every node's `Output`, the relations schema-qualified, and the
WHERE as the scans' `Filter` with its columns prefixed, as PostgreSQL 15.19
prints it. Before, any WHERE made such a plan fall back to the plain form.

#### Fixed

- `EXPLAIN VERBOSE` of a nested loop with a single-relation WHERE prints
  its output lists and a prefixed `Filter`. Fixes RP-966 in part
  (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a nested loop with a where]).
