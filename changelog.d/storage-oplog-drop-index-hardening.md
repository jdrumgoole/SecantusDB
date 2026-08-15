### dropIndex is race-safe and a failed oplog emit no longer freezes change streams

Two storage-layer robustness fixes on the Python server.

`drop_index` and `drop_all_indexes` took only the global storage lock, not
the per-collection lock that CRUD writers coordinate against — the same gap
that was closed for `create_index` but left open on the drop side. A write
landing between an index's entry-table snapshot and its deletion could
survive as an orphaned entry row. Both now take `_coll_lock` before `_lock`,
the canonical order.

The bare (autocommit) oplog-emit path deregistered its minted sequence range
at the end of the method with no surrounding `try`/`finally`. Every DDL write
goes through it, and if the cursor-write loop or the opportunistic prune
raised — a WiredTiger write error or `WT_ROLLBACK` under contention, both
expected — the minted range was never removed from the in-flight set, so the
change-stream visible tail clamped at that sequence for the life of the
process and change streams server-wide silently stopped advancing. The
mint-to-deregister region is now exception-safe.

#### Fixed

- `drop_index` / `drop_all_indexes` now hold the per-collection lock, so a
  concurrent insert/update/delete can't leave an orphaned index entry behind
  (#635).
- A failed bare oplog emit no longer strands its minted sequence range in the
  in-flight set — the change-stream visible tail recovers instead of freezing
  server-wide until restart (#714).
