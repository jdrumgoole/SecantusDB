### Rust PG server: EXPLAIN picks a BitmapOr, an index-only scan and a composite index's later columns

EXPLAIN of one analysed user table's scan now weighs every index path
PostgreSQL 15.19's `create_index_paths` builds: a BitmapOr for an `OR` of
indexed arms, an Index Only Scan where the index holds every column the
statement reads, and an index's quals on its later key columns.

#### Fixed

- `explain select * from k where a = 5 or a = 6` printed a `Seq Scan`; it
  is now a `Bitmap Heap Scan` over a `BitmapOr` of two `Bitmap Index Scan`s
  on `k_pkey`, with PostgreSQL's costs, `Recheck Cond` and each arm's
  `Index Cond`. An arm holding a clause no index serves rechecks only the
  index conditions and filters on the whole `OR`, as PostgreSQL prints it;
  an arm over a composite index's two columns is one scan of it. A JSON
  plan names the BitmapOr's scans `Member` plans.
- `explain select a + 1 from k where a = 5` printed an `Index Scan` of
  zeros; it is now an `Index Only Scan` with `cost_index`'s estimate. A
  computed select list's columns count towards the index holding every
  column read, and `enable_indexonlyscan = off` makes it a plain index
  scan, as `check_index_only` does.
- `explain select * from n where a = 7 and b = 7` over `n_ab (a, b)`
  printed a `Seq Scan`; it is now an `Index Scan using n_ab` with both
  quals in its `Index Cond`, priced by `btcostestimate`'s bound quals (the
  later columns' quals after a run of equalities). An `Index Cond` lists
  its quals in the index's column order.

Found and filed: RP-382 (a Sort, Aggregate or Limit over a user table's
index path prints zeros) and RP-383 (EXPLAIN ANALYZE of a user table's
index-only or bitmap scan leaves out `Heap Fetches`, `Heap Blocks` and the
bitmap nodes' actual rows).

Fixes RP-378 (tests/backlog/test_rp_explain_costs.py::test_plan[a bitmap or of a key's two equalities], tests/backlog/test_rp_explain_costs.py::test_plan[an index-only scan under a computed select list], tests/backlog/test_rp_explain_costs.py::test_plan[an index scan of a composite index's two columns])
