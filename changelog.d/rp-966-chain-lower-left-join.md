### EXPLAIN VERBOSE prints a join chain with a LEFT JOIN below an inner join

A left-deep chain of hash joins with a LEFT JOIN at any level, not only the topmost, now prints every node's `Output` under VERBOSE, as PostgreSQL 15.19 does: each LEFT JOIN hashes its nullable relation alone, and its ON's columns stay in a join's `Output` only while a join above still needs them.

#### Fixed

- `EXPLAIN (VERBOSE)` of a hash-join chain whose LEFT JOIN sat below another join printed the plain plan. Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a three-way join over a left join], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a chain of two left joins]).
