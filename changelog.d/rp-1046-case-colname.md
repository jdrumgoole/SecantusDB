### Rust PG server: an unaliased CASE column is named by its ELSE result

PostgreSQL names `case when ... else c.relnatts end` `relnatts`, after its
ELSE result, when that result has a name of its own (a column, a call, a
keyword such as `coalesce`, a scalar subquery's column); only a CASE whose
ELSE is a literal, or that has none, is `case`. The Rust PostgreSQL server
named every such column `case`, in the row description, in a derived
table's or CTE's columns (so an outer `select relnatts` was 42703, and a
SQL function body read its parameter in the column's place), and in
`pg_get_viewdef`'s `AS`. A cast above a CASE now keeps that name too
(`(case ... else c.relnatts end)::int8` is `relnatts`), and one above a
CASE that is only `case` takes the type's name (`int8`), as
`FigureColnameInternal` does.

#### Fixed

- An unaliased CASE column takes its ELSE result's name, and keeps it
  through a cast, as on PostgreSQL 15.
- A nested cast of an unnamed value (`(1::int4)::int8`) is named for the
  outer type in view text and derived-table columns.

Fixes RP-1046 (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_names_a_case_column_by_its_else_column)
