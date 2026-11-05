### Rust PG server: EXPLAIN picks a user table's index path as PostgreSQL does

EXPLAIN of one analysed user table's scan now makes PostgreSQL 15.19's
`set_cheapest` choice between the sequential scan, an Index Scan and a
Bitmap Heap Scan, instead of printing whatever node the storage planner
chose.

#### Fixed

- `explain select * from k where a = 5 and b = 'v5'` printed a `Seq Scan`
  with the sequential scan's cost; it is now `Index Scan using k_pkey`
  with the key's `Index Cond` and a `Filter`, as PostgreSQL prints it. So
  is a range on the key (`a < 3`) and a composite index's leading column.
- An equality on a secondary index's column (`n_b`, 100 of 5,000 rows) is
  now a `Bitmap Heap Scan` over a `Bitmap Index Scan` with PostgreSQL's
  costs.

Not modelled, so they keep the priced `Seq Scan`: an `OR` (a BitmapOr), two
indexes on different leading columns (a BitmapAnd), a partial index, an
index-only scan, and a select list that computes an expression.

Fixes RP-339 (tests/backlog/test_rp_explain_costs.py::test_plan[an index scan with a filter])
