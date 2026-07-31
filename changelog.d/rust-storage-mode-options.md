### The async oplog stack graduates to first-class options

The Rust server's storage write-path modes — the background oplog drainer,
non-logged oplog tables, and the mongod-style log-only-the-oplog data mode
with its stable-checkpoint cadence — were until now reachable only through
process-wide `SECANTUS_*` environment variables. They are now real,
per-store options at every layer: a `StorageOptions` struct on the storage
crate, `RustServer(oplog_async=…, oplog_nonlogged=…, data_nonlogged=…,
checkpoint_seconds=…)` kwargs on the embedded handle, and `--oplog-async` /
`--oplog-nonlogged` / `--data-nonlogged` / `--checkpoint-seconds` flags plus
matching `[storage]` TOML keys on the `secantusd-rs` daemon. Unset options
defer to the environment variables, so existing env-driven workflows are
unchanged; an explicit option wins for that store only.

Two async-mode gaps closed on the way: an async store now prunes its oplog
opportunistically from write volume (the every-1000-emits cadence the sync
path always had — previously an async store only pruned on explicit calls),
and `create_archive` drains the oplog queue before its checkpoint so a
backup taken under the async drainer can no longer miss acknowledged writes.

#### Added
- `secantus_storage::StorageOptions` + `Storage::open_with_options` — per-store
  `wt_config` / `durable` / `oplog_async` / `oplog_nonlogged` / `data_nonlogged` /
  `checkpoint_seconds`; `None` defers to the matching `SECANTUS_*` env var.
- `RustServer` kwargs `oplog_async` / `oplog_nonlogged` / `data_nonlogged` /
  `checkpoint_seconds` (embedded handle).
- `secantusd-rs` flags `--oplog-async` / `--oplog-nonlogged` / `--data-nonlogged` /
  `--checkpoint-seconds N` and `[storage]` keys `oplog_async` / `oplog_nonlogged` /
  `data_nonlogged` / `checkpoint_seconds` (Rust-daemon-only; `secantusd-py`
  rejects them).

#### Fixed
- Async-mode change streams could surface **pre-open events**: a write
  acknowledged before `watch()` could still be queued at the drainer, so the
  open position (seeded at the drainer's watermark) sat below it and the event
  leaked into the new stream (pymongo's `test_kill_cursors`, async-only). The
  open path now waits (bounded) for the drainer to reach the minted tail
  captured at open (`Storage::oplog_open_seq`); sync mode is unchanged — an
  open transaction's pinned visible tail is already the correct open position,
  and flushing there would block opens behind long transactions.
- Async-oplog stores never pruned the oplog from write volume; the drain path
  now mirrors the sync emit path's opportunistic every-1000-emits prune.
- `create_archive` under the async drainer could snapshot before queued oplog
  entries landed; it now calls `flush_oplog()` first.
- `docs/rust/embedded.md` documented `replica_set_name=None` as defaulting to
  the replica-set persona; the embedded handle's default is a plain standalone
  `hello` (pass `replica_set_name="secantus"` for change streams).
