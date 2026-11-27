### Rust PG server: a database-wide ANALYZE that changes a catalog's size removes the relation cache init files

A committed relation cache invalidation of a relation an init file holds
now removes the shared init file and the database's, as PostgreSQL's
`RelationCacheInitFilePreInvalidate` does, so the next backend builds them
again. A database-wide `ANALYZE` now runs `vac_update_relstats`' dirty
check on every built-in catalog. It compares the catalog's blocks, rows and
all-visible blocks against what was last recorded, and it invalidates the
catalog's `pg_class` row (and its indexes' rows when the row count moved)
only when something changed. A repeat `ANALYZE` with nothing new, a
single-catalog `ANALYZE` or a `CREATE TABLE` leaves the files, as on
PostgreSQL 15.20.

#### Fixed

- After a database-wide `ANALYZE` on a fresh server, the next backend
  makes the cold start's heap scans of `pg_class`, `pg_attribute` and
  `pg_authid`, as PostgreSQL does. Before, it read the init files the
  `ANALYZE` should have removed.

Fixes RP-1097 (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_removes_the_init_files)
