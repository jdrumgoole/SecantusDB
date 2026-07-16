### Concurrent writers: heartbeats stop writing the oplog meta row; conflict retries are unbounded

Two follow-ups from the concurrency harness. `current_cluster_time()` no
longer persists the oplog meta row on every call — it runs on every
`hello` reply under the replica-set persona (driver heartbeats) and on
change-stream high-water-mark minting, so it was a single-row write
hotspot; restart monotonicity is now guaranteed structurally (recovery
bumps the cluster clock one second past the meta hint, the oplog tail,
and the wall clock, so mints that were never persisted can't be
re-minted). And the non-transaction write-conflict retry loop loses its
5-second deadline: real mongod's `writeConflictRetry` loops until the
write goes through, so a client never sees `WriteConflict` (112) for a
plain write — ours now matches, logging a warning during long retry
stretches. Post-fix sweeps show zero client-visible conflict errors at
1–8 concurrent writers.

#### Fixed

- `Storage.current_cluster_time()` is write-free; recovery bump keeps
  restart cluster time strictly monotonic (regression-tested with a
  simulated crash).
- `_retry_write_conflicts`: unbounded retry with capped backoff outside
  user transactions (inside one, conflicts still surface immediately as
  mongod's statement-time `WriteConflict`).
