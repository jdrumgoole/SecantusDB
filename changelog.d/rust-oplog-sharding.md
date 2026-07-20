### Rust server: sharded oplog closes most of the write-scaling gap to mongod

The Rust server's oplog is now sharded across sixteen WiredTiger btrees instead
of one. A single append-only oplog table made every concurrent writer rendezvous
on that table's rightmost B-tree page, and that one contention point — not the
storage lock — was what held multi-writer throughput flat. Spreading the oplog
across sixteen trees (each write routed by `seq % 16`) lets writers append in
parallel: measured single-writer insert throughput rises from ~16k to ~29k
docs/s and eight-writer scaling from 0.60× to 2.47× — both now approaching the
oplog-off ceiling and, for the first time, mongod's own scaling curve.

The oplog stays a single logical, strictly-ordered stream: every reader (change
streams, `local.oplog.rs`, resume-token lookup, retention prune, PITR archive /
restore, and recovery) does a k-way merge across the shards in seq order, so
change-stream ordering, resume tokens, and point-in-time recovery are byte-for-byte
unchanged. Because the shard layout is an on-disk format change, the Python server
also learned to read, recover, and prune a Rust-written store's sharded oplog, so
cross-server backup and PITR portability keep working in both directions.

#### Changed
- Rust server oplog persisted across sixteen shard tables
  (`secantus_oplog_sh0..15`), routed per entry by `seq % 16`; all oplog readers
  merge the shards (plus the legacy single table) in seq order.
- Python `Storage` oplog readers (`read_oplog`, `oplog_floor_seq`,
  `find_seq_for_ts`, prune, recovery, PITR archive) merge the sharded oplog so the
  Python server can read/recover/prune a Rust-written store; Python writes stay on
  the legacy single table.

#### Fixed
- Oplog retention prune deletes each doomed row from both its shard and the legacy
  table (WiredTiger cursors are overwrite-mode, so a `remove()` of an absent key
  silently succeeds — a shard-then-legacy fallback would have leaked pruned rows on
  a Python-written store).
