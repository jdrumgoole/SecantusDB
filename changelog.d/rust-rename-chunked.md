### Rust server: renaming a huge collection can no longer wedge the engine

`renameCollection` re-keyed every row in one WiredTiger statement
transaction — the same unbounded-dirty-content livelock class as the
(already fixed) one-transaction drop purge, and the last DDL path that
could wedge the engine on a collection larger than the cache's dirty
budget. The rename is now a chunked two-phase move that reuses the drop
tombstones: tombstone the destination, copy the rows across in bounded
transactions (fresh RecordIds preserving insertion order, index entries
and unique claims rebuilt per batch), then one small switch transaction
registers the destination, unregisters the source, moves the tombstone,
and emits the rename oplog entry, and the source's rows purge in bounded
batches. Both crash windows recover through the existing open-time
tombstone recovery — on either server — as a plain drop: a crash
mid-copy purges the partial destination (the rename never happened); a
crash after the switch purges the leftover source (it did). A
deterministic regression test renames a collection larger than a small
cache — the shape that previously spun forever.

#### Fixed
- Rust server: `renameCollection` of a collection larger than the WT
  cache's dirty budget livelocked the engine (one unbounded re-key
  transaction + unbounded write-conflict retry); now a chunked two-phase
  move with crash-safe tombstone recovery. Inside a user transaction the
  atomic single-transaction path remains, bounded by the transaction
  dirty-budget guard. The batched copy also drops the old whole-collection
  in-memory materialization.
