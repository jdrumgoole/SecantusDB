### Idle connections can no longer pin WiredTiger's transaction horizon

The pgjdbc conformance lane's two-hour hang had a second, deeper cause beyond
the idle-in-transaction timeout shipped previously: a connection whose last
statement left its cached WiredTiger session with a positioned cursor held an
*implicit* transaction — invisible to every PostgreSQL-level accounting — and
pinned the storage engine's oldest-transaction horizon while it idled. Every
write after that pin kept its history unevictable, so per-operation cost grew
linearly with churn until a 100k-row TRUNCATE stalled in page reads and wedged
the server. The wedge needed a specific mix of prior traffic to arm, which is
why it only appeared mid-way through the full pgjdbc suite.

Both wire servers now call the new `Storage.release_thread_snapshot()` before
blocking for the next client message: `WT_SESSION.reset()` releases the
snapshot and every cursor position in one cheap call, so an idle connection
holds nothing by construction. Inside an open user transaction the release is
a deliberate no-op — a transaction's pinned snapshot is its semantics, and the
transaction-lifetime / idle-in-transaction timeouts bound that case. The
previously-deterministic pgjdbc wedge reproduction now runs clean with the
pinned-transaction-range statistic flat at zero.

#### Added

- `Storage.release_thread_snapshot()` — releases the calling thread's WT read
  snapshot and cursor positions; called by both the PG and Mongo wire servers
  at the end of every request, before the idle wait.
- `tests/test_storage_snapshot_release.py` — statistics-backed regression
  tests: a positioned cursor measurably pins the horizon and the release
  clears it; the release is a no-op inside a user transaction; a wire-level
  invariant that an idle PG connection never accumulates a pinned range.

#### Fixed

- An idle connection's stale read snapshot no longer degrades all later
  writes without bound (the pgjdbc `CopyLargeFileTest` wedge / 2-hour CI lane
  timeout). The Rust server's equivalent idle-session behaviour is tracked as
  a follow-up in `tasks/backlog.md`.
