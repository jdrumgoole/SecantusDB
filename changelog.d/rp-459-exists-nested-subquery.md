### A subquery in an EXISTS subquery's select list is analysed

PostgreSQL parse-analyses the whole select list of an EXISTS subquery before
its planner throws the list away, so a subquery inside it that names a
column or table that does not exist is an error, while one that would only
fail when run is never run. The select list's subqueries are now planned
without running before the list is dropped, and the errors parse analysis
raises are the statement's.

#### Fixed

- Rust PG server: `select exists (select (select nosuch))` is 42703 at 31
  and `select exists (select 1 + (select nosuch))` 42703 at 35 (each
  answered `true`); `select exists (select (select 1/0))` stays `true`.
- Fixes RP-459 (tests/backlog/test_rp_fromless_analysis.py::test_exists_target_subquery_is_analysed)
