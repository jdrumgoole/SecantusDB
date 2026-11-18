### pg_get_viewdef prints pg_stat_database

`pg_get_viewdef('pg_stat_database')` now returns PostgreSQL's text rather
than NULL. The view's subquery unions an integer constant with an oid
column, and the view printer did not know that PostgreSQL resolves an
integer and an oid to oid; it now does, in set operations as elsewhere, and
a set operation's column now takes the wider of two integer types.

#### Fixed

- Rust PG server: `pg_get_viewdef` of `pg_stat_database`, and of a user view
  whose set operation meets an integer and an oid, was NULL / the text as
  written. Fixes RP-798, trimmed (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_pg_stat_database,
  tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_of_a_union_of_an_integer_and_an_oid)
- Filed RP-1046 (a CASE column's name) and RP-1047 (an IN list holding a
  column), found while working RP-798.
