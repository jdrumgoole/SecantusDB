### EXPLAIN VERBOSE prints joins a WHERE constant fixes

A join whose key a WHERE constant fixes, as in `... from e join k on e.a = k.a where e.a = 3`, is planned as a Nested Loop with no join clause. Each table's scan is filtered by the constant. Under VERBOSE such a plan now prints as PostgreSQL 15.20 does, for two tables or a chain of three or more. Each scan returns its table's columns, or the key alone for an Index Only Scan, and prints its quals prefixed (`Filter: (e.a = 3)`, `Index Cond: (k.a = 3)`). Each join keeps a column only while an ON clause that reads it still reaches a table outside that join.

Before this change, VERBOSE printed the two-table plan with no `Output` lists and its quals bare, and drew the chain as Hash Joins, a plan PostgreSQL does not choose.

#### Fixed

- `EXPLAIN (VERBOSE)` of a two-table join whose class a WHERE constant fixes printed no `Output` and unprefixed quals, and of such a chain printed Hash Joins. Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a join a class constant fixes], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a class constant join over an index only scan], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a three-way join a class constant fixes], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a four-way join a class constant fixes]).
- Filed while working it: RP-1137 (a constant written on the left of its equality), RP-1138 (such a join under an ORDER BY or LIMIT, or beside a second ON equality), RP-1139 (such a scan's price beside another qual).
