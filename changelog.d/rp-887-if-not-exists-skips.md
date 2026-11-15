### CREATE TABLE IF NOT EXISTS over an existing table checks nothing

`CREATE TABLE IF NOT EXISTS` over a table that already exists now skips with
PostgreSQL's notice before looking at its column list, as
`transformCreateStmt` does: an unknown type, a column named twice or a bad
literal default no longer raise 42704, 42701 or 22P02.

#### Fixed

- The Rust PG server answered `create table if not exists t (a nosuchtype)`
  over an existing `t` with an error; it now succeeds.
  Fixes RP-887 (tests/backlog/test_rp_found_rp881.py::test_if_not_exists_over_a_table_checks_nothing)
