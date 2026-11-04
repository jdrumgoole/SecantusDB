### Rust PG server: ANALYZE measures array columns, catalogs' included

A database-wide `ANALYZE` kept `initdb`'s statistics for every catalog array
column, so after `create table ... with (fillfactor=50)` `pg_class.reloptions`
still had one distinct value and `initdb`'s null fraction. A user table's
`int4[]` or `text[]` column got no `pg_stats` row at all, and every filter on
it printed `cost=0.00..0.00 rows=0 width=0` in `EXPLAIN`. Both now follow
PostgreSQL 15.19's `array_typanalyze`.

#### Fixed

- ANALYZE of a one-dimensional array whose element type's statistics are
  modelled (the integers, `oid`, `bool`, the floats, `text`, `name`,
  `"char"`) records the scalar statistics under `array_eq` / `array_lt`.
  Keys order as `array_cmp` does. Width is the array datum's size: each
  element padded to its `typalign`, and a 1-byte header when the array
  fits one. It also records `compute_array_stats`' element slots: the most
  common elements by Lossy Counting (`STATISTIC_KIND_MCELEM`), and the
  histogram of distinct-element counts (`STATISTIC_KIND_DECHIST`).
- A catalog's collatable column records its statistics under the `C`
  collation, its `attcollation`.
- `pg_class.reloptions` of `pg_stats`, `pg_stats_ext` and
  `pg_stats_ext_exprs` is `{security_barrier=true}`, as `system_views.sql`
  creates them.
- `EXPLAIN` estimates `=`, `<` and `<>` against an analysed array column
  from its most common values.

Fixes RP-313 in part (tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_an_array_column,
tests/backlog/test_rp_catalog_reads.py::test_a_database_wide_analyze_measures_array_elements,
tests/backlog/test_rp_catalog_reads.py::test_an_analyzed_array_column_is_estimated).
RP-313 stays open for `aclitem[]`, the `reg*` types, `int2vector` /
`oidvector` and `pg_node_tree`. Filed RP-367: `EXPLAIN` of `@>`, `&&` and
`= ANY (col)` prints `Filter: (...)` with zero cost.
