### Rust PG server: `SELECT *` with no FROM is 42601, as on PostgreSQL

A bare `*` in a select list with no FROM has nothing to expand, and
PostgreSQL's `ExpandAllTables` refuses it with 42601 `SELECT * with no
tables specified is not valid` at the `*`. The Rust PostgreSQL server
answered `0A000 this target is not supported yet` instead, and in an EXISTS
subquery it dropped the list unanalysed, so `select exists (select *)` ran
and answered `true`.

#### Fixed

- `select *`, `select 1 union select *`, `select (select *)` and `select
  exists (select *)` are 42601 at the `*`, met in the target list's order:
  a missing column or a cast literal's input error to its left still comes
  first, and the `*` is reported ahead of anything in WHERE or ORDER BY.
  Measured on PostgreSQL 15.19.

Fixes RP-446 (tests/backlog/test_rp_fromless_analysis.py::test_star_with_no_from)
