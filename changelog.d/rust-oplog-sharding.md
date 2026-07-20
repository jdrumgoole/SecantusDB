### Rust server: much faster concurrent writes (sharded oplog + bounded prune)

The Rust server's write path got two changes that together lift insert throughput
substantially. First, the oplog is now sharded across sixteen WiredTiger btrees
instead of one: a single append-only oplog table made every concurrent writer
rendezvous on that table's rightmost B-tree page, and that one contention point —
not the storage lock — held multi-writer throughput flat. Each write *batch* is
routed to one shard by its start sequence, so concurrent writers append in parallel
while each batch stays a contiguous sequential append.

Second, and the larger win, the opportunistic oplog retention prune no longer
re-scans the entire oplog on the write path. A `sample` profile of a single-writer
insert loop showed that scan was ~77% of the write-path CPU — every thousand
writes walked the whole oplog. The prune now keeps a live entry count, so under the
size cap with recent entries it does a single timestamp read instead of a full
walk, and when trimming it touches only the bounded set of oldest rows it will
actually drop.

Measured back-to-back on an idle machine (before vs after), single-writer insert
throughput rose ~1.5× (14.5k → 22.2k docs/s). The bigger change is under
concurrency: the old write path *lost* throughput as writers were added (it peaked
around two writers and degraded past that), whereas the new one scales — peaking
near 46k docs/s at four writers (~2× the single-writer rate) and holding there at
eight, roughly 4–5× the old path's throughput at the same writer count. The oplog
stays a single logical,
strictly-ordered stream — change-stream ordering, resume tokens, and point-in-time
recovery are byte-for-byte unchanged — and because the shard layout is an on-disk
format change, the Python server also learned to read, recover, and prune a
Rust-written store's sharded oplog, so cross-server backup and PITR portability
keep working in both directions.

#### Changed
- Rust server oplog persisted across sixteen shard tables
  (`secantus_oplog_sh0..15`), each write batch routed to one shard by its start
  sequence; all oplog readers merge the shards (plus the legacy single table) in
  seq order via a k-way merge.
- Opportunistic oplog prune bounded via a maintained live-entry count: an early-out
  (one timestamp read) when under the cap with the oldest row still in-window, and a
  bounded walk of only the doomed rows otherwise — replacing the full-oplog scan
  that dominated the single-writer write path.
- Python `Storage` oplog readers (`read_oplog`, `oplog_floor_seq`,
  `find_seq_for_ts`, prune, recovery, PITR archive) merge the sharded oplog so the
  Python server can read/recover/prune a Rust-written store; Python writes stay on
  the legacy single table.

#### Fixed
- Oplog retention prune deletes each doomed row from its exact source table
  (WiredTiger cursors are overwrite-mode, so a `remove()` of an absent key silently
  succeeds — a shard-then-legacy fallback would have leaked pruned rows on a
  Python-written store).
