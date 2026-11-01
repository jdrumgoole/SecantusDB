### Rust PG server: the built-in catalogs have `initdb`'s statistics (RP-311)

`pg_statistic`, and so `pg_stats`, held no row for a built-in catalog;
PostgreSQL 15.19's `initdb` ANALYZEs them and every database starts with
those 404 rows over 38 catalogs. They are now PostgreSQL's own rows,
dumped from a fresh 15.19 cluster in heap order and listed before any
table's.

#### Fixed

- `pg_statistic` and `pg_stats` list `initdb`'s rows for the built-in
  catalogs, in every database, each value as PostgreSQL prints it (an array
  column's values quoted as their own text: `{"{security_barrier=true}"}`).
  All 404 `pg_stats` rows match 15.19's.

Fixes RP-311 (tests/backlog/test_rp_catalog_reads.py::test_builtin_statistics)

Filed while working it: RP-313 (a database-wide ANALYZE does not analyse
the catalogs) and RP-314 (EXPLAIN of a scan of a catalog has no estimate).
