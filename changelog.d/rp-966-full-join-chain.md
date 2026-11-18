### EXPLAIN VERBOSE prints a chain of FULL and LEFT joins

A chain of hash joins that mixes FULL and LEFT joins now prints every node's `Output` under `EXPLAIN VERBOSE`, as PostgreSQL 15.19 does, wherever the FULL JOIN sits, so long as only outer joins are above it. Before, only a FULL JOIN at the top of the chain did, and a LEFT JOIN whose ON read the nullable side of an outer join below it fell back to the plain plan. An outer join's ON never reduces the joins below it; only an inner join's ON does.

#### Fixed

- `EXPLAIN VERBOSE` of a chain of FULL joins, of a LEFT JOIN over a FULL JOIN, and of a FULL JOIN over a LEFT JOIN prints each node's `Output`.

RP-966, in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a chain of full joins]); RP-1064 filed.
