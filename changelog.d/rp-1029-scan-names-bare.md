### EXPLAIN names a scan's relation the way PostgreSQL does

A scan of a temporary table, or of a table in a schema on the search path
other than `public`, used to print the server's storage key as its name and
then repeat the table's name as an alias: `Seq Scan on pg_temp_22.t t`.
EXPLAIN now names the relation bare, `Seq Scan on t`, adds the alias only
when one was written, and qualifies the name only under VERBOSE, where the
session's own temporary schema reads `pg_temp`, as in PostgreSQL 15.

#### Fixed

- Rust PG server: EXPLAIN names a scan of a temporary or non-`public`
  table bare, with its alias only when it differs (`ExplainScanTarget`).
  Fixes RP-1029 (tests/backlog/test_rp_explain_costs.py::test_plan[a scan of a temporary table names it bare], tests/backlog/test_rp_explain_costs.py::test_plan[a scan of an aliased temporary table names it and its alias], tests/backlog/test_rp_explain_costs.py::test_plan[a scan of a table in a schema on the search path names it bare])
