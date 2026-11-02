### Rust PG server: a database-wide ANALYZE measures the catalogs' columns of a modelled type

A plain `ANALYZE` (or `VACUUM ANALYZE`) with no table named now analyses the
built-in catalogs as well, as PostgreSQL's `get_all_vacuum_rels` does: each
catalog column whose type's statistics are modelled -- `oid` (new here, also
for user tables), the integers, `bool`, the floats and `text` -- is measured
over the catalog's rows as they stand, and its `pg_statistic` row replaces the
one `initdb` left (RP-311). Columns of other types (`name`, `"char"`, `reg*`,
arrays) keep `initdb`'s rows for now; RP-313 stays open for them.

#### Fixed

- After `create table rp313_t (a int); analyze`, the top of `pg_class.oid`'s
  histogram is the new table's oid, as on PostgreSQL 15.19, instead of
  `initdb`'s.
- An `oid` column of a user table gets statistics from ANALYZE.

Fixes RP-313 (part) (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_the_catalogs)
