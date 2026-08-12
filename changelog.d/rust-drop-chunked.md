### Rust server: dropping a huge collection can no longer wedge the engine

Dropping a collection ran its whole row purge as one WiredTiger statement
transaction. Because collections share the sharded document tables, a drop
is a row-by-row purge — and a collection whose delete volume exceeds the
cache's dirty budget got a cache-pressure `WT_ROLLBACK`, which the write-
conflict retry loop re-ran forever while the eviction threads spun. That is
the livelock the 2026-08-11 concurrency sweep hit: a drop that sat for 40+
minutes at full CPU, survived client disconnect, and ignored SIGTERM. The
same unevictable-dirty-content class was already fixed for batch inserts,
updateMany, and deleteMany; drop (and dropDatabase) were the remaining
unbounded transactions.

Drops are now chunked and two-phase. A small first transaction unregisters
the collection, writes a drop tombstone, and emits the drop oplog entry —
after it commits the namespace is gone for every reader and writer. The row
purge then runs in bounded 4000-row transactions and finally clears the
tombstone. A crash mid-purge is finished at the next open, before any
traffic can re-create the name, so leftover rows can never resurface inside
a re-created collection. Inside a user transaction, drops keep the old
atomic single-transaction path, which the transaction dirty-budget guard
(`TransactionTooLargeForCache`) already bounds. A deterministic regression
test drops a collection larger than a deliberately small cache — the exact
shape that previously wedged — and a recovery test pins the crash-left
tombstone path.

#### Fixed
- Rust server: `drop` / `dropDatabase` of a collection larger than the WT
  cache's dirty budget livelocked the engine (unbounded purge transaction +
  unbounded write-conflict retry); now chunked, with crash-safe tombstone
  recovery at open.

#### Added
- `table:secantus_drop_tombstones` (additive to the shared on-disk layout):
  pending-drop markers that make the chunked purge crash-safe.
