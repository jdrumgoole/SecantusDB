# Backup and point-in-time recovery

The Rust server has an oplog-backed backup/recovery model — hot backup
archives over the wire, plus offline point-in-time restore. The
concept-level docs are in the main tree's
[Recovery](https://secantusdb.com/docs/recovery.html) page; this is the
Rust-server surface.

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
(there is no `secantusAdmin.restoreToTimestamp` wire command):

```
secantusd-mdb restore --source PATH --target-dir PATH [--to-timestamp SECS[,ORD]]
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
secantusd-mdb restore \
    --source ./secantus-data \
    --target-dir ./secantus-data-restored \
    --to-timestamp 1752694800
# 3. Start fresh on the restored directory:
secantusd-mdb --port 27017 --storage-path ./secantus-data-restored
```

## Durability knobs

WiredTiger journals every commit and syncs the journal every 100 ms, as
`mongod`'s journal flusher does; a journaled write (`j: true`, or the
default `w: "majority"` of the one-member replica set) waits for that sync.
After a crash the server recovers as `mongod` does: WiredTiger replays its
journal, the oplog is truncated after `local.replset.oplogTruncateAfterPoint`,
and the oplog after the last stable checkpoint is replayed into the data.
So, as on `mongod`, a write acknowledged with `{w: 1, j: false}` in the
last ~100 ms before a crash can be lost; a journaled one never is.
`--sync-on-commit` fsyncs every commit and acknowledges every write as a
journaled one, at a substantial throughput cost.

## A data directory `mongod` wrote

`secantusd-mdb` opens a data directory `mongod` 8.2 wrote as a one-member
replica set, stopped cleanly or killed: a killed one is recovered as
`mongod`'s own restart recovers it. It serves the set under the set's own
name, and `mongod` can open the directory again afterwards (and opens one
SecantusDB wrote). Never both at once: WiredTiger's lock makes the second
server exit. What it cannot serve yet is refused by name before anything is
written: a set of more than one member, a time-series collection, an index
build `mongod` left unfinished, `2d`, `2dsphere`, `hashed`, text and
wildcard indexes, users and roles, and a feature compatibility version
other than 8.2.
