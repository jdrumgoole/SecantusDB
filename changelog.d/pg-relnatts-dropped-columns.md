### Rust PG server: `pg_class.relnatts` counts dropped columns

After `ALTER TABLE ... DROP COLUMN`, `pg_class.relnatts` now counts every
attribute number the table has used, the dropped column's slot included, as
PostgreSQL does. Before, it counted only the live columns, so it disagreed
with the table's own `pg_attribute` rows (which already kept the dropped
slot): 1 where PostgreSQL 15.19 answers 2.

#### Fixed

- `pg_class.relnatts` is the table's highest attnum, dropped columns
  included, not its count of live columns.
  Fixes RP-293 (tests/backlog/test_rp_catalog_reads.py::test_found[relnatts counts dropped columns])
