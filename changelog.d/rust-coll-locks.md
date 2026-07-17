### The Rust server's writers stop queueing behind each other

Writes on the Rust server now serialise per collection instead of per
server. Each collection gets its own write lock (created on first
reference, stable across drop-and-recreate); inserts, updates, deletes,
replaces and TTL prunes take only their collection's lock, so writers to
different collections run in parallel where previously every write in the
process queued on one global mutex — the flat ~0.5× concurrency scaling
the three-way benchmark measured. DDL (index builds, create/drop/rename,
collMod) takes the global lock plus the affected collection lock(s), so an
index build excludes in-flight writes on its namespace — which the Rust
server genuinely needs, because its write path is autocommit per operation
and WiredTiger would not surface a DDL-vs-write overlap as a conflict the
way the Python server's per-statement transactions do.

Getting the exactness guarantees right under real overlap took two more
pieces, both ported from the Python server's concurrency work. Every write
statement now runs inside its own WiredTiger snapshot transaction —
without one, an update could read a document in one implicit transaction
and write it in another, and a competitor committing in between was
silently overwritten with a value computed from the stale read (a lost
update the new stress tests catch reliably). The statement transaction
also makes each write atomic: document row, index entries, natural-order
rows and oplog rows commit or vanish together, closing a
crash-mid-statement window that could previously leave a dangling index
entry. And a write that loses a race retries: statement-level WT_ROLLBACK
and the bare-EINVAL commit-time conflict (a competitor marking the
transaction rollback-only after its last operation) both map to the typed
WriteConflict, which plain writes retry unbounded — matching mongod's
writeConflictRetry, with a warning logged every few seconds of continuous
retrying — while statements inside a user transaction surface it
immediately so the client sees mongod's statement-time WriteConflict with
the TransientTransactionError label.

#### Changed

- Rust server: CRUD writes serialise per collection (own lock per
  namespace); DDL takes the global lock plus the affected collection
  lock(s); the opportunistic oplog prune moved to its own mutex so the
  write path never takes the global lock. Lock-order rules are documented
  on the storage struct.
- Rust server: every write statement runs in its own WiredTiger snapshot
  transaction — statement atomicity (doc row + index entries +
  natural-order rows + oplog rows commit together) and write-conflict
  detection across the statement's read-modify-write.
- Rust server: tailable change-stream waiters are woken after the
  statement (or user-transaction) commit makes the oplog rows visible,
  not at emit time inside the still-open transaction.

#### Fixed

- Rust server: a plain write racing a multi-document transaction on the
  same document can no longer be lost to a stale-snapshot read-modify-
  write — the statement transaction turns it into a detected conflict and
  the write retries to completion, so both increments land.
- Rust server: a commit-time transaction conflict (bare EINVAL from
  WiredTiger after a competitor marked the transaction rollback-only)
  now surfaces as the retriable WriteConflict instead of a generic
  internal error, matching the Python server's #444 mapping.
- Rust server: two callers racing the lazy collection-UUID mint can no
  longer mint different UUIDs for the same namespace — the mint runs
  under the collection write lock with a double-check, and the
  already-minted fast path is a lock-free read.

#### Added

- `crates/secantus-storage/tests/concurrent_writes.rs` — cross-collection
  writer storms (exact counts), same-collection `$inc` hammers (exact
  final value), unique-index races (exactly one winner, typed loser
  errors), `createIndex` under write load (index-routed reads must reach
  every document), a plain-write-vs-transaction race (retries to
  completion, both effects land), and a transaction-vs-transaction
  statement conflict (typed WriteConflict, no retry inside the
  transaction).
