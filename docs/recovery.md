# Backup & point-in-time recovery

SecantusDB supports two recovery models:

1. **Snapshot backup / restore** — a consistent copy of the whole database at the
   moment the backup was taken.
2. **Point-in-time recovery (PITR)** — rebuild the database as it was at *any*
   target time, by replaying the oplog forward.

Both are **offline restores**: they produce a fresh data directory that you then
point a *new* server at (`secantusdb --storage-path <dir>` /
`SecantusDBServer(storage_path=<dir>)`). Hot in-place restore over a live
WiredTiger connection isn't supported — real `mongod` restores work the same way
(stop, swap the data directory, start).

## Snapshot backup

`Storage.create_archive` forces a WiredTiger checkpoint and tars the consistent
file set into a single `.tar.gz`. Over the wire it's the `secantusAdmin.backupArchive`
command:

```python
from pymongo import MongoClient

admin = MongoClient("mongodb://127.0.0.1:27017")["admin"]
admin.command({"secantusAdmin.backupArchive": 1, "outputPath": "/backups/db.tar.gz"})
```

Because the oplog lives in the same WiredTiger connection as the data, the
archive is **self-contained** — it carries the oplog up to the checkpoint. Each
archive also embeds a small `pitr-manifest.json` describing the oplog range it
can recover to (floor / head seq and timestamps, and whether the oplog still
reaches genesis).

Restore the snapshot with the `secantusdb-restore-archive` tool (extract into a
fresh directory) or the `secantusAdmin.restoreArchive` command.

## Point-in-time recovery

PITR is *snapshot + oplog replay*: SecantusDB records a mongod-shaped oplog
(`local.oplog.rs`), and recovery replays it into a fresh store, stopping at a
target timestamp. The result is the database exactly as it was at that instant —
documents, in-place updates, deletes, collection options (`capped` / `size` /
`max` / `validator` / `viewOn` / …), and index/`collMod`/rename DDL all
reconstructed by replaying through the ordinary write paths.

### CLI

```bash
# Recover to a wall-clock time:
secantusdb restore --source /backups/db.tar.gz \
                   --target-dir /restore/at-1430 \
                   --to-time 2026-06-17T14:30:00Z

# Or to a precise cluster timestamp (seconds[,ordinal]):
secantusdb restore --source /path/to/stopped-data-dir \
                   --target-dir /restore/exact \
                   --to-timestamp 1781716542,7

# With neither --to-time nor --to-timestamp, the whole oplog is replayed
# ("latest"). Then start a server on the result:
secantusdb --storage-path /restore/at-1430
```

`--source` is a backup `.tar.gz` **or** a stopped server's data directory (a
live one can't be opened — WiredTiger holds a single-writer lock). `--target-dir`
must be a fresh path.

### Wire command

`secantusAdmin.restoreToTimestamp` exposes the same operation for admin tooling:

```python
admin.command({
    "secantusAdmin.restoreToTimestamp": 1,
    "source": "/backups/db.tar.gz",      # archive or stopped data dir
    "targetDir": "/restore/at-1430",
    "toTimestamp": Timestamp(1781716542, 7),   # or "toTime": <datetime>; omit for latest
})
```

### Transactions

Every statement in a multi-document transaction shares one commit timestamp, so
the timestamp cut is always **all-or-nothing** for a transaction — a recovery
point never lands in the middle of one.

## The recovery window (v1)

The current implementation replays onto an **empty** base, which is exact
whenever the source oplog still reaches genesis — i.e. it hasn't been pruned
from the front. The recovery window is therefore the **oplog retention window**.
Tune it for the horizon you need:

```bash
secantusdb --oplog-retention-seconds 604800 --oplog-max-entries 5000000   # ~1 week
```

(or the `[oplog]` section of `secantusdb.toml`). The rule of thumb: *keep enough
oplog and you can rewind to any point in it.*

If the oplog has been pruned past genesis, restore **fails loudly** rather than
silently rebuilding a partial database — recovering to a time before the oplog
floor would need a base snapshot to start from, which is the deferred v2
(continuous oplog archiving + scheduled base snapshots; see `tasks/backlog.md`).

### Limitations

- The restored data directory starts a **fresh oplog timeline** — the replayed
  history isn't carried into the target, so a change stream on the restored
  server resumes from the restore point, not from before it (like
  `mongorestore`).
- See [Change streams](change-streams.md) for the oplog model and
  [Compatibility](compatibility.md) for the broader divergence list.
