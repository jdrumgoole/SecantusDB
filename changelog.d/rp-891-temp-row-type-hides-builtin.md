### A temporary table's row type hides a built-in type of its name

PostgreSQL searches the session's temporary schema for a type ahead of
`pg_catalog` unless the search path places `pg_temp` after it. After
`create temp table text (a int)`, `'x'::text` now goes to the row type's
input and fails with `malformed record literal`, and `('(1)'::text).a`
returns 1, as on PostgreSQL 15.

#### Fixed

- Rust PG server: an unqualified type name finds a temporary row type before
  a built-in type of the same name.
  Fixes RP-891 (tests/backlog/test_rp_found_rp565.py::test_a_temporary_row_type_hides_a_built_in_type)
