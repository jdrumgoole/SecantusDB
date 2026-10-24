### Rust PostgreSQL server: a sequential scan reads rows in ctid order

An unordered read of a table now returns its rows in heap order, as
PostgreSQL 15.19's sequential scan does. After an UPDATE, the new version
of a row is the page's last tuple, so `select a from t` over rows 1, 2, 3
where row 2 was updated reads 1, 3, 2, here as on PostgreSQL. Every answer
that depends on input order is computed over that order:
- `LIMIT` and `OFFSET` without `ORDER BY`;
- a hash join's outer side;
- `string_agg` and a float `sum`.

#### Fixed

- Fixes RP-192
  (`tests/backlog/test_rp_batch46.py::test_scenario[a scan reads rows in ctid order]`,
  and `tests/backlog/test_rp_batch47.py::test_scenario[an unordered select reads the heap in ctid order]`,
  `[a table with a primary key reads in ctid order too]`,
  `[a hash join probes its outer side in ctid order]` and
  `[a transaction's own updates are read in ctid order]`).
  - **When the scan sorts.** Rows are sorted by `ctid` only once a table's
    tuples have left the stored order, through an UPDATE or a `CLUSTER`.
    Until then the stored order already is the heap's, and the read costs
    nothing extra.
  - **Covered paths.** The materialised and streamed SELECT, the streamed
    join and the streamed aggregates all read through it. A streamed read
    sorts in bounded memory, as an `ORDER BY` does.
  - **Restarts.** After a restart, whether a heap is out of order is
    derived from the stored rows' ctids.
  - **What keeps its own order.** An index scan keeps the index's order. A
    partitioned table's rows keep the order they had, since each leaf has
    ctids of its own.

#### Known

- RP-193, extended: PostgreSQL reaches a HOT-updated row through its
  chain's root, so a bitmap heap scan over a primary-key range reads it at
  the root's place. This server reads it at its new ctid
  (`tests/backlog/test_rp_batch47.py::test_scenario[a bitmap heap scan reads a HOT-updated row at its root]`).
