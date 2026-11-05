### Rust PG server: a database-wide ANALYZE measures `aclitem[]` columns

A plain `ANALYZE` now re-measures a catalog's `aclitem[]` columns, such as
`pg_class.relacl`, as PostgreSQL 15.19 does. It also works for a user
table's `aclitem[]` column. The type has `=` but no `<`, so the statistics
come from `compute_distinct_stats`: its track list, its distinct count and
its most common values.

#### Fixed

- After a GRANT and an `ANALYZE`, `pg_stats` for `pg_class.relacl` showed
  `initdb`'s null fraction and frequencies. It now shows the ones measured
  over the catalog's rows.
- Seven `information_schema` relations (`_pg_foreign_data_wrappers`,
  `_pg_foreign_servers`, `_pg_foreign_table_columns`, `_pg_foreign_tables`,
  `_pg_user_mappings`, `sql_parts`, `transforms`) had a PUBLIC SELECT
  grant in `relacl`. `initdb` gives them no ACL, so `relacl` is now NULL.

Part of RP-313 (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_an_aclitem_array).
The `reg*`, `int2vector` / `oidvector` and `pg_node_tree` columns stay
open, pinned by `test_a_database_wide_analyze_measures_a_regproc_column`.
