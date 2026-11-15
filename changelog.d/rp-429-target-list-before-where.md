### A missing output column is reported before the WHERE's errors

PostgreSQL analyses a SELECT's target list before its WHERE clause, so
`select nosuch from e where 'x'` reports the missing column, not the
WHERE's bad boolean. The Rust PostgreSQL server now does the same.

#### Fixed

- Rust PG server: a missing column in the target list is 42703 ahead of a
  22P02, 42804 or 42883 raised by the WHERE clause.
  Fixes RP-429 (tests/backlog/test_rp_where_types.py::test_target_list_is_analysed_before_where)
