### The oplog is held to its size in bytes, and a store is initiated as mongod initiates a set

The Rust servers' oplog is now bounded as mongod bounds its own: by its `size`
in bytes, truncated from the oldest end in whole entries, never past the stable
checkpoint's timestamp. The entry-count cap is gone (`--oplog-max-entries`,
`[oplog] max_entries`), and so is `secantus_oplog_meta`, the table that held
the oplog's ident, the newest position pruned and the stable checkpoint's
position. What a one-member replica set keeps now sits where mongod keeps it:
the oplog's catalog entry, the `initiating set` no-op that begins the oplog,
the `local.replset.*` collections and WiredTiger's own recovery timestamp. This
is stages 4b-5 and 4b-6 of the storage model; the store format is 6, and a
store written by an earlier build is refused with the usual message.

#### Added

- `--oplog-size-mb N` (`[oplog] size_mb`, mongod's `--oplogSize`): the size of
  a new store's oplog. The default is mongod 8.2.11's: 192 MB on macOS (measured:
  a fresh `--replSet` mongod's `local.oplog.rs` has `maxSize` 201326592), else 5%
  of the free disk, at least 990 MB and at most 50 GB. An existing store keeps
  the size in its oplog's catalog entry, which `replSetResizeOplog` rewrites
  (`size` megabytes times 1048576, the fraction kept) and `collStats.maxSize`
  and `listCollections` report.
- A new store is initiated before it serves: the oplog begins with
  `{op: "n", ns: "", o: {msg: "initiating set"}}` (no `t`, as mongod writes it),
  and `local.replset.minvalid`, `.oplogTruncateAfterPoint`, `.election`,
  `.initialSyncId`, `local.system.rollback.id` and `config.system.indexBuilds`
  hold mongod's documents in mongod's shape (`tools/probes/mongod_replset_state.py`,
  `tests/fixtures/storage_model/mongod-8.2.11-replset-state.json`). The `local`
  tables are logged where mongod logs them (all but `replset.minvalid`).
  `rollbackId` rises by one after an unclean shutdown, as mongod's does.
- `Storage::oplog_reaches_genesis` (the oplog's oldest entry is the
  `initiating set` no-op: mongod's own mark of an oplog nothing was truncated
  from) replaces `oplog_pruned_through`.

#### Changed

- **The oplog is bounded by bytes.** The sweep takes the oldest whole entries
  off until the oplog is at or under its size. The total is kept as mongod
  keeps its truncate markers' (`live_count` / `live_bytes`: read at open from
  the size storer, else one walk; added to when a write commits, so a
  rolled-back statement or a retried conflict adds nothing; taken from by a
  truncation), so no write and no sweep counts the table. In a store whose data
  tables are not logged the sweep still stops below the stable checkpoint.
- **Pre-images expire against the oplog's oldest entry**, as mongod's remover
  does, not against what this run's sweeps happened to remove.
- **The store's first checkpoint is taken at its creation**, as mongod takes
  one at the end of `replSetInitiate`, so the catalog entry naming the oplog is
  durable before any write is accepted. That replaces the oplog's ident being
  recorded in a second table because the catalog is not logged: a kill -9
  before the first periodic checkpoint finds the oplog by its catalog entry and
  replays its logged entries (`tests/test_crash_recovery.py`,
  `tests/test_rust_binary_crash_durability.py`, the kill loop).
- **The recovery point is WiredTiger's recovery timestamp** (`get=recovery`),
  the stable timestamp a checkpoint stores with itself, where the periodic
  checkpoint thread used to write a marker row after each checkpoint.
- A PITR segment's `prev` is 0 when its first entry is the `initiating set`
  no-op, else the end of the newest segment in the archive, else just before
  its first entry (the pruned-position row went with `secantus_oplog_meta`;
  see RM-737).
- `collStats` of `local.oplog.rs` reads its count and size from the totals
  above, not by walking the oplog.
- `docs-rust/running.md`, `docs-rust/embedded.md`, `docs/configuration.md`,
  `docs/recovery.md`, `docs/change-streams.md` and `secantusd.toml.example`
  describe the byte size.

#### Removed

- `--oplog-max-entries`, `[oplog] max_entries` and `Storage::set_oplog_max_entries`
  (`set_oplog_max_bytes` replaces it).

#### Open

- RM-735 (the election term and a `new primary` entry on each start), RM-736
  (`oplogTruncateAfterPoint`'s refresh cadence and recovery's truncation),
  RM-737 (the PITR archive's chain without a pruned mark) and RM-738
  (`--oplog-retention-seconds`, a time limit mongod does not have).
