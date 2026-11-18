### EXPLAIN VERBOSE prints a three-way join with a WHERE

A left-deep chain of hash joins over three or more relations now keeps its VERBOSE detail when the query has a WHERE whose conjuncts each read one relation: every node prints its `Output`, and each filtered scan prints its `Filter` prefixed with the relation's name, as PostgreSQL 15.19 does. A column read only by a filter stays out of a hashed scan's `Output`.

#### Fixed

- `EXPLAIN (VERBOSE)` of a three-way hash-join chain with a per-relation WHERE printed the plain plan. Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a three-way join with a where], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a three-way join filtering the hashed relation]); RP-1059 filed.
