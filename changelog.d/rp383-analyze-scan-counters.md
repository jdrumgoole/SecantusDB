### Rust PG server: EXPLAIN ANALYZE counts an index-only scan's heap fetches and a bitmap scan's pages

`EXPLAIN ANALYZE` of a user table now prints the counters PostgreSQL 15.19
prints for its index scans: an Index Only Scan's `Heap Fetches`, a Bitmap
Heap Scan's `Heap Blocks: exact=` and its Bitmap Index Scan's own `actual
rows` (and, in the structured formats, `Exact Heap Blocks` / `Lossy Heap
Blocks`).

They come from PostgreSQL's own mechanism: each page of a user table's heap
model now carries its visibility map bit, set by VACUUM when every tuple on
the page is visible to every snapshot and cleared by any insert, update or
delete on the page, and persisted with the heap.

#### Fixed

- An index-only scan's `Heap Fetches` counts the returned tuples on pages
  the visibility map leaves unmarked: all of them before a VACUUM, none
  after it, and those on a page a later write cleared.
- A bitmap scan prints the heap pages its bitmap named and its index scan's
  tuples.
- Where the index entries a scan visits are not just the live rows (a dead
  version, dead line pointer or HOT chain not yet vacuumed), the counters
  are still left out rather than guessed (RP-384).

Fixes RP-383 (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a user table's index-only scan], tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a user table's bitmap scan], tests/backlog/test_rp_explain_costs.py::test_vacuum_sets_and_a_write_clears_the_visibility_map)
