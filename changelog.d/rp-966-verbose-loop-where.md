### EXPLAIN VERBOSE of a nested loop whose WHERE compares both relations

An inner join's ON and WHERE clauses are one list of join quals, as PostgreSQL's planner treats them. A WHERE comparison of a column of each relation now moves into a parameterised inner index scan's `Filter` beside the ON's other clauses, where it used to print as a second `Join Filter` line on the loop, and over a Materialize it joins the ON's conjuncts in one `Join Filter`. Under VERBOSE both plans now print every node's `Output` and prefix the filter's columns, as PostgreSQL 15.19 does.

#### Fixed

- `EXPLAIN` of a nested loop into a parameterised index scan prints a cross-relation WHERE conjunct as the scan's `Filter`, and a Materialize loop prints one `Join Filter` of the ON and WHERE conjuncts; `EXPLAIN VERBOSE` prints both plans' `Output` lists.

Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a nested loop whose where reads both relations], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a materialized loop whose where reads both relations])
