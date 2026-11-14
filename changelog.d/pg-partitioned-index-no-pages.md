### A partitioned table's indexes show no pages

A partitioned table's index has no storage of its own, and the Rust
PostgreSQL server now shows it in `pg_class` as PostgreSQL does: 0 pages
and 0 tuples, for the parent's indexes and for a partitioned partition's
copies of them alike, and still 0 and 0 after VACUUM, ANALYZE, TRUNCATE or
REINDEX. It answered 1 page for each primary key and for the parent's own
indexes, and 0 pages with -1 tuples (never built) for a partitioned
partition's copies of the others.

#### Fixed

- `secantus-pg`: a partitioned table's index is never recorded by a build,
  VACUUM or ANALYZE (`record_index_size`), as `index_update_stats` and
  `vac_update_relstats` never reach a relation without storage, and its
  `pg_class` row reads as `index_create` left it, 0 pages, 0 tuples.
- Fixes RP-931 (tests/backlog/test_rp_found_rp921.py::test_a_partitioned_tables_indexes_have_no_pages,
  tests/backlog/test_rp_found_rp921.py::test_maintenance_leaves_a_partitioned_tables_indexes_without_pages).
