### A chain of inequality joins under a LIMIT is priced in EXPLAIN

A chain of three or more tables joined by inequalities, read under a
`LIMIT`, already ran in the order PostgreSQL's search keeps, but EXPLAIN
printed its Limit and every node below it at zero cost, without the
Materialize nodes over each inner. It now prints the Limit's
`adjust_limit_rows_costs` estimate over the priced Nested Loops and
their Materializes, as PostgreSQL 15.19 does.

#### Fixed

- EXPLAIN of a chain of inequality joins under a `LIMIT` prices the
  Limit, each Nested Loop and each Materialize. The `OFFSET`, `ORDER BY`,
  multi-ON and bushy chains stay open.
  Fixes RP-919, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a chain of inequality joins under a limit])
