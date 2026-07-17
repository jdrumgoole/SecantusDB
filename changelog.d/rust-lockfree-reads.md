### The Rust server's reads stop queueing behind writers

Every read the Rust server served used to take the same global storage lock
as every write — concurrent readers serialised not just against writers but
against each other. Read-only storage methods (`find`, `count`, the `_id`
point lookup, collection scans, listers, planners and stats) now run
lock-free: each call's own WiredTiger session gets a consistent MVCC view
without blocking, so reads no longer queue behind a bulk insert and a mixed
read/write workload stops paying the writer's lock hold. All mutable Rust
state lives in WiredTiger tables or under the dedicated oplog mutex, so the
lock was buying readers nothing — correctness under concurrency is carried
by the storage schema instead (a fixed set of shared tables that DDL never
drops) plus the invariant that index-routed candidates are always
re-verified by the exact matcher and doc fetches tolerate not-found.

Making that invariant airtight surfaced three write-ordering fixes worth
having on their own. Index maintenance for updates is now a set diff — an
update inserts only the entry keys the new document adds and removes only
the ones it drops, so a `$set` of an unindexed field performs zero
index-table operations (the old scheme deleted and rewrote every entry,
opening a window where a document vanished from an index whose value the
update never touched). `createIndex` backfills its entry rows before
writing the registry row, so a reader can never route through a
half-built index and miss documents. And every delete-shaped path (delete,
TTL prune, capped eviction) removes the document row first and its index
entries after, so a stale entry resolves to a skipped not-found rather
than an index miss of a still-live document.

#### Changed

- Rust server: read-only storage methods no longer take the global storage
  lock — reads run concurrently with writes and with each other under
  WiredTiger MVCC. The lock now serialises only writes and DDL.
- Rust server: update index maintenance writes the set *difference* of
  entry keys (additions before the doc-row write, removals after) instead
  of delete-all-then-rewrite; updates that don't change indexed values do
  no index writes at all.

#### Fixed

- Rust server: `createIndex` now makes the index-registry row the commit
  point of a fully-backfilled index (entries first, registry last), so a
  concurrent reader can no longer route through a half-built index and
  return incomplete results.
- Rust server: delete-shaped writes (delete, TTL prune, capped-collection
  eviction) remove the doc row before its index/natural-order entries, so
  a concurrent index-routed read can no longer miss a document that is
  still live.

#### Added

- `crates/secantus-storage/tests/concurrent_reads.rs` — four reader
  threads hammer `find` / `findOne` / scans / counts / `listIndexes`
  against a live writer doing replaces, delete/re-insert churn and
  drop/recreate index churn; every served document must decode and match
  the filter it was returned for.
