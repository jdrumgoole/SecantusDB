### The oplog is `local.oplog.rs`, one table keyed by timestamp

The Rust servers' oplog is now what mongod keeps: the capped collection
`local.oplog.rs`, one WiredTiger table (`collection-<UUID>`, mongod 8.2.11's
config and `_mdb_catalog` row) whose RecordId is the entry's `ts`. The sixteen
shard tables, the legacy oplog table and the asynchronous oplog drainer are
gone, and so is the `--oplog-async` option. This is the first step of phase 4b
of the storage model: every reader of the oplog (change streams, resume tokens,
`awaitData`, retryable writes, pre-images, PITR, replay and restore, crash
recovery, the PostgreSQL server's two-phase commit) now names a position by its
timestamp, and a tailable cursor on `local.oplog.rs` sees new entries. The
store format is 5; a store written by an earlier build is refused with the
usual message.

#### Added

- `tests/test_rust_crash_kill_loop.py`: `kill -9`s `secantusd-mdb` and
  `secantusd-pg` under a write burst, 50 times each in the phase gate, and
  checks that every acknowledged write is present, that no transaction or
  `insertMany` is half applied and, for MongoDB, that a change stream opened
  before the kill resumes.
- `tools/probes/mongod_oplog_batching.py` and `mongod_oplog_prepare.py`, with
  their fixtures: which writes mongod 8.2.11 logs as one `applyOps`, its
  `partialTxn` chain, and a prepared transaction's entries.

#### Changed

- Store format 5: `local.oplog.rs` replaces `secantus_oplog_sh*`,
  `secantus_oplog` and the oplog `state` row; the clock resumes from the
  newest entry at open.
- PITR archive segments are named `oplog-<first>-<last>-<prev>.seg` and chain
  by abutting (`prev` is the entry a segment follows); a store's
  `oplogPrunedThrough` replaces "the floor is seq 1" as "the oplog reaches
  genesis".

#### Fixed

- RM-135 (`--oplog-async` as a default) is closed by removing the option: the oplog is
  written in the writer's own transaction, as mongod's is (Joe's decision of
  2026-11-16).
- A store reopened within the second of its last write could resume its clock
  behind WiredTiger's stable timestamp and refuse to open (EINVAL); the clock
  now also resumes past WiredTiger's recovery, stable and oldest timestamps
  (`reopening_a_timestamped_store_again_and_again_never_commits_behind_wiredtiger`).

#### Removed

- `--oplog-async`, `[storage] oplog_async`, `SECANTUS_OPLOG_ASYNC` and the
  `oplog_async` argument of the embedded `RustServer`; the `rust-async` arm of
  `bench/concurrency.py`.
