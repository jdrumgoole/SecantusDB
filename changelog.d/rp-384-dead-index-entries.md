### EXPLAIN ANALYZE counts a dead version's index entries, as PostgreSQL does

`secantusd-pg` now prints an index-only scan's `Heap Fetches`, a Bitmap
Index Scan's rows, a Bitmap Heap Scan's `Heap Blocks` and an index scan's
`Rows Removed by Filter` while the table holds dead versions not yet
vacuumed, where it used to leave each counter out.

#### Fixed

- After an UPDATE or DELETE, EXPLAIN ANALYZE printed none of these scan
  counters until a VACUUM. They are now read from each B-tree's page model,
  which keeps a dead version's entry under the key it was indexed with
  until VACUUM's index pass removes it or a scan marks it dead to all. The
  first index-only scan after a committed update prints `Heap Fetches: 2`
  and the next prints 1, as on PostgreSQL 15.20. A Bitmap Index Scan counts
  the dead entries, which no bitmap scan marks. An index scan's Filter
  counts only the live rows it fetched. The counts are taken before the
  statement runs, so a scan's own marking does not hide the entries it
  read.
- Fixes RP-384, in part (`tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an index-only scan over a dead version]`,
  `tests/backlog/test_rp_explain_costs.py::test_scan_counters_over_dead_versions`).
  Left open under RP-384: a bitmap that outgrows `work_mem` and goes
  lossy. Files RP-1120: a range read through a B-tree index marks no dead
  entries, because the executor takes it for a bitmap scan.
