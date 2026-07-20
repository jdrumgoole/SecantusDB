### Rust server: sharded oplog improves multi-writer write scaling

The Rust server's oplog is now sharded across sixteen WiredTiger btrees instead
of one. A single append-only oplog table made every concurrent writer rendezvous
on that table's rightmost B-tree page, and that one contention point — not the
storage lock — was what held multi-writer throughput flat. Spreading the oplog
across sixteen trees — each write *batch* routed to one shard by its start
sequence — lets concurrent writers append in parallel while keeping each batch a
contiguous sequential append. In a back-to-back A/B, eight concurrent writers
gained ~67% throughput and per-writer scaling improved from 0.27× to 0.57×.

This is a **tradeoff**: single-writer insert throughput drops ~20%, because a lone
writer has no append contention to relieve and sharding only adds routing overhead
plus scatters its batches across several trees rather than one cache-hot page. The
change is aimed at concurrent-writer workloads; single-writer-dominated ones are
slightly slower.

The oplog stays a single logical, strictly-ordered stream: every reader (change
streams, `local.oplog.rs`, resume-token lookup, retention prune, PITR archive /
restore, and recovery) does a k-way merge across the shards in seq order, so
change-stream ordering, resume tokens, and point-in-time recovery are byte-for-byte
unchanged. Because the shard layout is an on-disk format change, the Python server
also learned to read, recover, and prune a Rust-written store's sharded oplog, so
cross-server backup and PITR portability keep working in both directions.

#### Changed
- Rust server oplog persisted across sixteen shard tables
  (`secantus_oplog_sh0..15`), each write batch routed to one shard by its start
  sequence; all oplog readers merge the shards (plus the legacy single table) in
  seq order via a k-way merge.
- Python `Storage` oplog readers (`read_oplog`, `oplog_floor_seq`,
  `find_seq_for_ts`, prune, recovery, PITR archive) merge the sharded oplog so the
  Python server can read/recover/prune a Rust-written store; Python writes stay on
  the legacy single table.

#### Fixed
- Oplog retention prune deletes each doomed row from both its shard and the legacy
  table (WiredTiger cursors are overwrite-mode, so a `remove()` of an absent key
  silently succeeds — a shard-then-legacy fallback would have leaked pruned rows on
  a Python-written store).
