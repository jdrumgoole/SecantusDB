### Rust PG server: a database-wide ANALYZE measures the catalogs' `int2vector` columns

A plain `ANALYZE` now re-measures `pg_index.indkey` (and any other
`int2vector` column) over the catalog's rows as they stand, as PostgreSQL
15.19 does. `int2vector` counts as an array to `type_is_array`, so it sorts
under `array_ops` (`array_eq`, `array_lt`) and gets the most common values,
a histogram and a correlation, printed in `int2vectorout`'s text (`{1,"1
2",...}`). Before, the column kept `initdb`'s statistics after DDL had added
an index.

#### Fixed

- Fixes RP-313, part (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_an_int2vector_column).
  `oidvector` and `pg_node_tree` remain open, with a new gap test on
  `pg_proc.proargtypes`.
