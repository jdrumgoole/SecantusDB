### Rust PG server: EXPLAIN ANALYZE counts a BitmapOr's arms

`EXPLAIN ANALYZE` of a bitmap scan over an `OR` now prints what
PostgreSQL 15.19 prints for it: the BitmapOr's own `actual rows=0`, each
arm's Bitmap Index Scan its index tuples, and the Bitmap Heap Scan's
`Heap Blocks: exact=` the pages of the arms' union.

#### Fixed

- A BitmapOr and its arms printed no actual rows, and the heap scan over
  it no `Heap Blocks`.
  Fixes RP-384, part (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a BitmapOr])
