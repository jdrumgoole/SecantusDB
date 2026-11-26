### Rust PG server: a join of two catalogs reads what PostgreSQL's chosen plan reads

psql's `\d <pattern>` joins `pg_class` to `pg_namespace`, and PostgreSQL
runs it as a nested loop. Each scan under the loop is built with its
relation's whole row, so the executor looks up every column's type, and
the inner `pg_namespace` scan stops at each outer row's match because its
oid is unique. `secantusd-pg`'s catalog-read model treated every such
join as a hash join. In `pg_stat_sys_tables` / `pg_stat_sys_indexes` that
showed as 4 `pg_namespace` tuples where PostgreSQL 15.20 reads 3, and 6
`pg_type_oid_index` scans where it makes 14.

#### Fixed

- The walk now picks the join method of a two-catalog join with
  PostgreSQL's join costing (`joincost::JoinRel`). Each side is priced as
  `EXPLAIN` prices a catalog scan, including a pattern's filter cost and
  `pg_table_is_visible`'s selectivity. The reads follow the chosen plan:
  whole-row scans under a nested loop or on a hash join's outer side, a
  unique inner stopping at the first match in `initdb`'s heap order, and
  no inner scan for an empty outer.
- A join's relations build their B-tree indexes' pathkeys
  (`has_useful_pathkeys`: a join clause in an equivalence class) without
  an ORDER BY of their own, and an ORDER BY column of any relation counts
  as its equivalence class.

Fixes RP-290, part (tests/backlog/test_rp_catalog_reads.py::test_gap[psql's describe query], tests/backlog/test_rp_catalog_reads.py::test_a_catalog_joins_method_decides_its_reads). Files RP-1140 (tests/backlog/test_rp_found_rp290.py::test_a_table_with_a_text_column_has_a_toast_table).
