### Rust PG server: EXPLAIN reads a catalog index-only

A catalog read whose index holds every column the statement reads now
prints PostgreSQL 15.19's `Index Only Scan`, priced by `cost_index` with
the heap's all-visible fraction, where it printed a zero-cost `Seq Scan`.

#### Fixed

- `explain select attname from pg_attribute where attrelid = 1259` is
  `Index Only Scan using pg_attribute_relid_attnam_index on pg_attribute
  (cost=0.28..4.96 rows=39 width=64)` with its `Index Cond`, as on
  PostgreSQL 15.19. `enable_indexonlyscan = off` stops the index-only path
  being built and `enable_indexscan = off` disables it with the plain
  index scan, as `check_index_only` and `cost_index` do.
- Several catalog indexes on the same leading column (`pg_attribute`'s
  two) no longer leave the scan unpriced: as in `choose_bitmap_and`, an
  index whose clauses another already covers is redundant, not ANDed.
- Fixes RP-337 (tests/backlog/test_rp_explain_costs.py::test_plan[a catalog read index-only])

#### Found

- RP-360: `EXPLAIN ANALYZE` of an index-only scan does not print `Heap
  Fetches`.
