### Rust PG server: EXPLAIN ANALYZE counts an index-only scan's heap fetches

Under `ANALYZE`, a catalog's `Index Only Scan` now prints PostgreSQL
15.19's `Heap Fetches` line, counted from the catalog's visibility map,
which `initdb` left behind and the catalog dump now records.

#### Fixed

- `explain (analyze, timing off, summary off) select attname from
  pg_attribute where attrelid = 1259` ends with `Heap Fetches: 0`, as on
  PostgreSQL 15.19. The index is walked for its key, and each tuple on a
  heap block whose all-visible bit `initdb` left clear is fetched, as
  `IndexOnlyNext` does. A row DDL added later counts as fetched, because
  its insert cleared its page's bit.
- The structured formats print `Actual Rows` and `Actual Loops` straight
  after the costs, before the node's quals, as `ExplainNode` does. An
  index-only scan adds `Rows Removed by Index Recheck: 0` and `Heap
  Fetches`.
- `tools/probes/pg15_system_catalog_dump.py` records each built-in
  catalog's visibility map (`\vm` in `pg15_catalog_heaps.txt`).
- Fixes RP-360 (tests/backlog/test_rp_explain_costs.py::test_plan[an index-only scan's heap fetches])
