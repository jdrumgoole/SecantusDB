### Rust PG server: a database-wide ANALYZE measures the catalogs' `oidvector` columns

A plain `ANALYZE` now re-measures `pg_proc.proargtypes` (and any other
`oidvector` column) over the catalog's rows as they stand, as PostgreSQL
15.19 does. `oidvector` has its own B-tree opclass, `oidvector_ops`, so it
sorts as `btoidvectorcmp` does (by length first, then each oid unsigned)
under `oidvectoreq` / `oidvectorlt`, and its values print in
`oidvectorout`'s text. Before, the column kept `initdb`'s statistics after a
CREATE FUNCTION had added a row.

#### Fixed

- Fixes RP-313, part (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_an_oidvector_column).
  The `pg_node_tree` columns and the catalogs' `relpages` / `reltuples`
  remain open.
