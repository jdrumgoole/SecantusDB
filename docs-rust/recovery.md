# Backup and point-in-time recovery

The Rust server has the same oplog-backed backup/recovery model as the
Python server — hot backup archives over the wire, plus offline
point-in-time restore. Archives are **portable between the two servers**:
a backup taken from either restores on either. The concept-level docs are
in the main tree's [Recovery](https://secantusdb.com/docs/recovery.html)
page; this is the Rust-server surface.

## Hot backup (over the wire)

The `secantusAdmin.*` commands work while the server is running:

```python
admin.command("secantusAdmin.backupArchive", archivePath="/backups/db.tar.gz")
admin.command("secantusAdmin.archiveBaseSnapshot")   # PITR v2 base snapshot
admin.command("secantusAdmin.restoreArchive", ...)
admin.command("secantusAdmin.pruneOplog")
admin.command("secantusAdmin.pruneTtl")
```

Pair `--oplog-archive-dir` (or `[oplog] archive_dir` in `secantusd.toml`)
with `archiveBaseSnapshot` to keep pruned oplog rows recoverable beyond the
live retention window.

## Point-in-time restore (CLI)

The Rust server's point-in-time restore is a subcommand of the binary
(the Python server additionally exposes it over the wire as
`secantusAdmin.restoreToTimestamp`):

```
secantusd-rs restore --source PATH --target-dir PATH [--to-timestamp SECS[,ORD]]
```

It rebuilds a fresh data directory as the database was at the target time
by replaying a stopped server's oplog forward:

- `--source PATH` — a **stopped** server's data directory, or an extracted
  backup archive. A live data directory can't be opened; WiredTiger holds
  a single-writer lock.
- `--target-dir PATH` — fresh directory to rebuild into. Start a new
  server on it afterwards.
- `--to-timestamp S[,O]` — recover to this cluster timestamp (seconds,
  optional ordinal). Omit to replay the whole oplog ("latest").
- `--preserve-oplog` — carry the replayed oplog onto the restored
  directory so a change stream there can resume from before the restore
  point. The default is a fresh oplog timeline (like `mongorestore`).

Typical flow:

```bash
# 1. Stop the server (or extract a backup archive).
# 2. Replay to the moment before the bad deploy:
secantusd-rs restore \
    --source ./secantus-data \
    --target-dir ./secantus-data-restored \
    --to-timestamp 1752694800
# 3. Start fresh on the restored directory:
secantusd-rs --port 27017 --storage-path ./secantus-data-restored
```

## Durability knobs

WiredTiger journals every commit; `--sync-on-commit` additionally fsyncs
per commit (the `writeConcern j: true` guarantee) at a substantial
throughput cost. Crash recovery is WiredTiger log replay on startup —
identical semantics to the Python server, because it is literally the same
storage engine.
