### A set operation in FROM types its columns

A FROM subquery or CTE built from `UNION`, `INTERSECT` or `EXCEPT` now has
typed columns, named by its leftmost arm and typed as the arms' common type,
so a comparison over one is judged as PostgreSQL judges it:
`select x = 1 from (select 'a'::text union select 'b') s(x)` is 42883
`operator does not exist: text = integer` at position 10, not a run-time
0A000 with no position.

#### Fixed

- Rust PG server: a set-operation FROM item's columns are typed by the
  operator check. Fixes RP-765 (tests/backlog/test_rp_found_rp756.py::test_a_set_operation_from_subquerys_column_is_typed)
