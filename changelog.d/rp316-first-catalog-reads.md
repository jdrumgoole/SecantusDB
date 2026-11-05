### Rust PG server: a new session's first reads of a built-in catalog match PostgreSQL's

`pg_stat_sys_indexes` now counts the same catalog reads PostgreSQL 15.19
makes when a new session first reads a built-in catalog.

#### Fixed

- `select count(*) from pg_am` now builds `pg_am_name_index` and
  `pg_am_oid_index`, as PostgreSQL does: its catalog-cache start-up never
  opens them (`touch_index` is false for `AMOID` / `AMNAME`), so the
  relation cache init file does not hold them.
- A query that an index-only scan of a B-tree could answer, such as a
  `count(*)` with no qual, now reads the index's leading-column
  statistics, as `build_index_paths` and `btcostestimate` do. This covers
  the `pg_statistic` reads of `count(*)` over `pg_am` and `pg_namespace`.
- Operator and function resolution no longer lets an implicit coercion
  take the I/O path to a string type. `find_coercion_pathway` allows that
  path only from `COERCION_ASSIGNMENT`, and an assignment now accepts an
  assignment cast. `where oid = 96` now makes PostgreSQL's 42 `pg_cast`
  scans, not 45.

Fixes RP-316 (tests/backlog/test_rp_catalog_reads.py::test_a_built_in_catalogs_first_reads)
