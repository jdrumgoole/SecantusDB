### EXPLAIN VERBOSE prints merge joins and joins under ORDER BY and LIMIT

Under VERBOSE, a two-relation merge join on column equalities now prints every node's `Output`, as PostgreSQL 15.20 does. A sorted side returns only the columns the join needs, and its `Sort Key` is prefixed. A side read in order from an index returns the table's columns, or the key itself for an Index Only Scan of a one-column primary key. A two-relation join under an ORDER BY of columns, a LIMIT or both prints its `Limit` and `Sort` the same way. Each ORDER BY column the select list lacks is a resjunk entry that every node down to the join returns.

#### Fixed

- `EXPLAIN (VERBOSE)` of a two-relation merge join, and of a two-relation join under a `Sort` or a `Limit`, printed the plain plan. Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a merge join], tests/backlog/test_rp_explain_costs.py::test_plan[explain verbose of a sorted hash join under a limit]).
