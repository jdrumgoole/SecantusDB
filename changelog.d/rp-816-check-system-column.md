### Rust PG server: a system column in a CHECK is 42P10

A CHECK constraint that named a system column -- `check (xmin = '1')`,
`check (ctid is not null)` -- was refused at CREATE TABLE as though the
column did not exist (42703), and ALTER TABLE ... ADD CHECK accepted it.
PostgreSQL refuses every system column but `tableoid` in a CHECK
(`scanNSItemForColumn` under `EXPR_KIND_CHECK_CONSTRAINT`).

#### Fixed

- CREATE TABLE and ALTER TABLE ADD CHECK refuse a system column other than
  `tableoid` with 42P10 `system column "xmin" reference in check constraint
  is invalid`, at the reference under CREATE TABLE and with no position
  under ALTER TABLE, in the same order against a missing column or a
  refused boolean argument as PostgreSQL 15.19.

Fixes RP-816 (tests/backlog/test_rp_found_rp811.py::test_a_system_column_in_a_check_is_42p10)

Files RP-859 (`tableoid` in a CHECK), RP-860 (a whole-row reference in a
CHECK), RP-861 (a CHECK's qualified column references) and RP-862 (a system
column in a generated column), each with a gap test in
`tests/backlog/test_rp_found_rp816.py`.
