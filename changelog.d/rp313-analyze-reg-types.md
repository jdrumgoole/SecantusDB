### Rust PG server: a database-wide ANALYZE measures the catalogs' `reg*` columns

A plain `ANALYZE` now re-measures the built-in catalogs' `regproc` (and
other `reg*`) columns, such as `pg_type.typinput`, over the catalog's rows
as they stand, as PostgreSQL 15.19 does: a `reg*` type sorts under `oid`'s
B-tree opclass (`oideq`, `oidlt`). Before, those columns kept `initdb`'s
statistics after DDL had added rows. A user table's `reg*` column is
measured the same way.

#### Fixed

- Fixes RP-313, part (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_a_regproc_column).
  `int2vector`, `oidvector` and `pg_node_tree` remain open, with a new gap
  test on `pg_index.indkey`.
