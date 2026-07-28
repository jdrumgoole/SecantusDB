### Lazy shard creation cuts Storage open cost ~2×, and a shutdown-hang fix

Opening a store eagerly created all ~37 WiredTiger tables up front — 16
per-collection document shards plus 16 oplog shards dominating — even for an
ephemeral store that touches a single collection. At ~10.6 ms per WT `create`
that is ~500 ms and 51 files per open, and under a highly parallel test run it
saturated disk I/O badly enough to stall (workers stuck in uninterruptible I/O
wait). Both servers now create shards **on demand**: a document shard is made on
first creation of a collection that hashes to it, and an oplog shard on first
write to it (the Python server, which never writes the sharded oplog, creates no
oplog shards at all). A fresh store now creates ~13 base tables plus only the
shards actually used, roughly halving open cost (open + one collection + insert
dropped from ~500 ms to ~300 ms; 51 files → 20). Every read / scan / merge / drop
/ rename / `$out` / delete path on both servers treats an absent shard as empty
(Python `_cursor_optional`; Rust `WtError::is_missing_table`), so a store written
with a subset of shards stays byte-compatible with an eager store and across
servers for backup / PITR — a missing shard simply reads as empty.

Separately, `Storage.close()` could hang forever inside WiredTiger's
`WT_CONNECTION->close`: it joined the background TTL sweeper (on by default,
`ttl_sweep_seconds=60`) with only a 2-second timeout and then tore WiredTiger
down anyway, so under load — when a sweep outran the 2 s budget — it closed the
sweeper's still-live WT session from the wrong thread and `conn.close()` blocked.
It now joins the sweeper and heartbeat threads to completion before any
WiredTiger teardown, and each loop closes its own thread-local session on exit.

#### Changed

- Both the Python and Rust servers create documents / oplog shard tables lazily
  (on first write; the Python server creates no oplog shards at all) instead of
  all ~37 eagerly at open, cutting Storage open cost ~2× and the per-open file
  count from 51 to ~20. Read, scan, merge, drop, rename, `$out`, and delete paths
  on both servers tolerate an absent shard, so the on-disk layout stays
  cross-server byte-compatible (a missing shard reads empty).

#### Fixed

- `Storage.close()` no longer hangs in `WT_CONNECTION->close` when the TTL
  sweeper or noop-heartbeat thread is active: the threads are joined to
  completion before WiredTiger teardown, and each closes its own thread-local
  WiredTiger session on exit rather than leaving it for a cross-thread close.

#### Internal

- Test harness: the session `_hang_watchdog` now routes its traceback to the
  per-worker crash file (`SECANTUS_FAULTHANDLER_DIR`) and stays armed through
  shutdown, so a shutdown-time wedge self-diagnoses instead of dying anonymously
  as "node down". Hang timeout tunable via `SECANTUS_HANG_SECONDS`.
