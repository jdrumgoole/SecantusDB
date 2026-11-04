### Rust PG server: EXPLAIN builds a catalog's bitmap scan

When a catalog's cheapest path is a bitmap heap scan, for example with
`enable_indexscan = off`, EXPLAIN now prints PostgreSQL 15.19's two nodes:
a `Bitmap Heap Scan` with its `Recheck Cond`, over a `Bitmap Index Scan` on
the index with its `Index Cond`, each with the cost PostgreSQL gives it.
Before this it printed a `Seq Scan` with zero cost.

#### Fixed

- `explain select oid from pg_class where relname = 'pg_class'` under
  `enable_indexscan = off` is `Bitmap Heap Scan on pg_class
  (cost=4.28..8.29 rows=1 width=4)` over `Bitmap Index Scan on
  pg_class_relname_nsp_index (cost=0.00..4.28 rows=1 width=0)`, as on
  PostgreSQL.

Fixes RP-338 (tests/backlog/test_rp_explain_costs.py::test_plan[a catalog read by a bitmap])
