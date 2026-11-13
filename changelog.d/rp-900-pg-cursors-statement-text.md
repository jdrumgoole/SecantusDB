### pg_cursors shows the DECLARE as it was sent

A cursor listed in `pg_cursors` now reports, in its `statement` column, the
query string its DECLARE arrived in, verbatim, as PostgreSQL does: the whole
text of a several-command query, the Parse text of an extended-protocol
statement with its `$1` placeholders, comments and spacing kept. The Rust
PostgreSQL server used to report a deparse of the cursor's query instead
(`SELECT 1` for `declare c cursor for select 1 -- x`).

#### Fixed

- Rust PostgreSQL server: `pg_cursors.statement` for a DECLAREd cursor is
  the portal's source text, the query string the statement came from
  (PostgreSQL 15.19's `PerformCursorOpen` copies `p_sourcetext`), read
  from the session's statement source. The planner no longer deparses the
  cursor's query at every DECLARE.
- Fixes RP-900 (tests/backlog/test_rp_found_rp744.py::test_pg_cursors_statement_is_the_declare_text).
  Regression test: tests/test_rust_pgserver_slice.py::test_pg_cursors_statement_is_the_source_text
  (simple, several-command, extended and psycopg server-cursor DECLAREs, and
  a WITH HOLD cursor).
- Filed RP-936: the extended protocol's unnamed portal is listed in
  `pg_cursors` as `POSTGRESQL_DEFAULT_NAME`
  (tests/backlog/test_rp_found_rp900.py::test_the_unnamed_portal_is_not_in_pg_cursors).
