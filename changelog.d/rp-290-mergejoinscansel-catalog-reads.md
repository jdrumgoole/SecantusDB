### A merge join's scan estimate counts the catalog reads PostgreSQL makes for it

The Rust PostgreSQL server's `pg_stat_sys_tables` and `pg_stat_sys_indexes`
count a statement's catalog reads as PostgreSQL 15 makes them. Costing a merge
join over a merge-joinable clause, such as psql's `\d` pattern query joining
`pg_class` to `pg_namespace`, now reads what `mergejoinscansel` reads. That is
the clause family's `<` and `<=` and their functions, each side's range from
its statistics, the histogram operator's families, and the probes of a catalog
column's index for its actual least and greatest values when the histogram
search reaches its first or last bin. Before, a merge join read only the
family's two operators.

#### Fixed

- `secantus-pg` (`catalog_reads.rs`, `statistic.rs`): `mergejoinscansel`,
  `get_variable_range`, `scalarineqsel`, `ineq_histogram_selectivity`,
  `comparison_ops_are_compatible` and `get_actual_variable_range` are walked
  for each merge-joinable join clause once the base relations are planned,
  over the column's `pg_statistic` row (`initdb`'s, a database-wide ANALYZE's,
  or the table's own). `\d rpc_t`'s first query now differs from PostgreSQL
  15.19 in 2 counters, down from 5.
  Fixes RP-290, in part (tests/backlog/test_rp_catalog_reads.py::test_a_merge_join_clauses_scan_estimate)
