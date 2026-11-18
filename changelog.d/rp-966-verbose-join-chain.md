### EXPLAIN VERBOSE prints a chain of three or more joined relations

`EXPLAIN VERBOSE` of a query joining three or more tables left-deep, by inner hash joins on column equalities, now prints every node's `Output` list as PostgreSQL does. Each join returns its outer side's columns and then its inner side's, keeping a column while a table of its equivalence class is still outside that join, so a key equated across all three tables is carried up to the last join that needs it.

#### Fixed

- Rust PG server: `EXPLAIN VERBOSE` of a left-deep chain of hash joins over three or more relations, with no WHERE, prints each node's `Output`. Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a join of three relations], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a join of four relations]); filed RP-1056 and RP-1057.
