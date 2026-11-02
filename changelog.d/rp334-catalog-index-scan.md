### Rust PG server: EXPLAIN reads a catalog by its index

`explain select oid from pg_class where relname = 'pg_class'` now prints
PostgreSQL 15.19's plan, `Index Scan using pg_class_relname_nsp_index on
pg_class  (cost=0.27..8.29 rows=1 width=4)` with `Index Cond: (relname =
'pg_class'::name)`, where it used to print a zero-cost Seq Scan. A clause
on the leading column of one of a catalog's B-tree indexes opens that
index's paths: the plain index scan is costed by `cost_index` over
`btcostestimate`, using the index's pages and height as `initdb` left
them. The cheapest path is kept, as `set_cheapest` keeps it, and the
`enable_*` settings add `disable_cost`.

#### Fixed

- EXPLAIN of a catalog read through its index prints an Index Scan with
  its estimate and `Index Cond`, with or without COSTS; a small catalog
  still reads sequentially, as on PostgreSQL. The JSON format lists an
  index scan's `Scan Direction`.
- A `name` column compared with an untyped literal is printed as `name =
  name` (`'pg_class'::name`), not cast to `text`. In EXPLAIN, an integer
  compared with an `oid` column is the folded constant `'1259'::oid`.

Fixes RP-334 (tests/backlog/test_rp_explain_costs.py::test_plan[a catalog read by its index])
Fixes RP-335 (tests/backlog/test_rp_explain_costs.py::test_plan[a name compared with a literal])
