### Rust PG server: EXPLAIN keeps an index-only scan after DELETE and VACUUM

A VACUUM of a table that had lost rows since the last VACUUM recorded it as
unmodelled, so the planner fell back to a Seq Scan where PostgreSQL 15.19
keeps `Index Only Scan`. VACUUM now takes `relallvisible` from the modelled
heap's own visibility map (`visibilitymap_count`), whatever rows left it.

#### Fixed

- `delete from k where a = 7; vacuum k` then `select a from k where a < 10`
  plans `Index Only Scan using k_pkey on k` with `Heap Fetches: 0`, as
  PostgreSQL does.

Fixes RP-385 (tests/backlog/test_rp_explain_costs.py::test_an_index_only_scan_after_a_delete_and_vacuum)
