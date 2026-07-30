### The Rust server's change streams get a real oplog visibility point

Concurrent writers on different collections could permanently lose a change
event. The Rust server's tailable cursors treated the highest *minted* oplog
seq as the readable tail, but a seq is minted inside its writer's still-open
transaction — so a writer on one collection could commit a *later* seq while
an earlier one was still in flight, and a change stream that polled in that
window advanced its resume position past the hole. When the in-flight
transaction then committed, its event sat behind the stream's position:
dropped from the live stream and unreachable on resume. Same-collection
writers never hit this (the per-collection lock serializes them), which is
why it survived every single-collection test.

The fix is the analogue of WiredTiger/mongod's `all_durable` timestamp: an
in-flight window tracks every minted-but-unresolved seq range, and readers —
`wait_for_oplog`, `read_oplog`, change-stream open positions, post-batch
resume tokens — are bounded by its floor. A commit releases its range and
the tail advances; a rollback releases it silently, leaving a permanent seq
hole the shard merge already tolerates, so an aborted transaction can never
stall the stream. `flush_oplog` in sync mode now genuinely waits for the
window to drain, and abandoned transaction handles release their ranges on
drop so a reaped session cannot pin the tail.

#### Fixed

- Rust server: change streams no longer lose events when writers on
  different collections commit out of oplog-mint order (live and on
  resume). New `Storage::oplog_visible_tail_seq()` is the bound every
  reader uses; three WT-level pinning tests and a cross-collection
  database-watch exactly-once test guard the invariant.

#### Added

- `tests/test_mongo_server_concurrency.py::test_db_change_stream_exactly_once_across_collections`
  — N per-collection writers under a database-wide watch, asserting
  exactly-once delivery (no duplicates, no losses), on both servers.
