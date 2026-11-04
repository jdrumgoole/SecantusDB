### Rust PG server: a database-wide ANALYZE measures the catalogs' `name` and `"char"` columns

A plain `ANALYZE` now re-measures the built-in catalogs' `name` columns
(`relname`, `attname`, `typname`, ...) and `"char"` columns (`relkind`,
`typtype`, ...) over their rows as they stand, as PostgreSQL 15.19 does,
instead of keeping `initdb`'s statistics for them. A user table's `name` and
`"char"` columns are measured too.

#### Fixed

- After a new table and `ANALYZE`, `pg_stats` shows it at the top of
  `pg_class.relname`'s histogram, and `relkind`'s most common values and
  frequencies match PostgreSQL's. The slots are recorded under `name`'s
  `=` / `<` (93 / 660) in the `C` collation and `"char"`'s (92 / 631).
  Part of RP-313 (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_a_name_column,
  tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_a_quoted_char_column);
  the other column types stay open. Filed RP-358: a catalog column's
  correlation follows this server's row order, not PostgreSQL's heap.
