# Backup & point-in-time recovery

SecantusDB supports two recovery models:

1. **Snapshot backup / restore** — a consistent copy of the whole database at the
   moment the backup was taken.
2. **Point-in-time recovery (PITR)** — rebuild the database as it was at *any*
   target time, by replaying the oplog forward.

Both are **offline restores**: they produce a fresh data directory that you then
point a *new* server at (`secantusd-mdb --storage-path <dir>`). Hot in-place restore over a live
WiredTiger connection isn't supported — real `mongod` restores work the same way
(stop, swap the data directory, start).

## The interface

The MongoDB server, `secantusd-mdb` ([see the servers](servers.md)),
provides:

- the `restore` subcommand, `secantusd-mdb restore`;
- the `secantusAdmin.backupArchive`, `secantusAdmin.restoreArchive` and
  `secantusAdmin.archiveBaseSnapshot` wire commands;
- the `--oplog-archive-dir` server flag.

The server stores data in WiredTiger and writes a **mongod-shaped oplog**, so
a backup carries everything a restore needs.

## How it works

PITR is **snapshot + oplog replay**. The pieces:

### The oplog

Every write a server accepts is recorded in a mongod-shaped operations log
(surfaced as `local.oplog.rs`), stored in the same WiredTiger connection as the
data. Each entry mirrors mongod's shape — `ts` (a `Timestamp(secs, ord)`), `op`,
`ns`, `ui` (collection UUID), `o`, `o2`, `wall` — and uses the same op codes:

| `op` | Meaning | `o` payload |
|------|---------|-------------|
| `i` | insert  | the inserted document |
| `u` | update  | `{$v: 2, diff}` for an operator update (a dotted-path `updateDescription`), or the whole replacement document |
| `d` | delete  | the deleted `_id` (in `o2`) |
| `c` | command (DDL) | `create` (with collection options as siblings), `createIndexes`, `dropIndexes`, `collMod`, `drop`, `dropDatabase`, `renameCollection` |
| `n` | no-op   | heartbeats / replica-set init — skipped on replay |

Because the oplog lives beside the data, a WiredTiger checkpoint captures both
consistently, and a backup archive is **self-contained**: it carries the oplog up
to the checkpoint.

### The applier

Recovery opens a stopped source (a backup archive or a stopped server's data
directory — a *live* directory can't be opened, WiredTiger holds a single-writer
lock), then replays the oplog forward into a fresh target, **stopping before the
first entry past the target time**. Each entry is applied through the server's
**ordinary write paths** — `i` inserts, `d` deletes by `_id`, `c` re-runs the DDL
— so the documents, indexes, collection options, and natural (insertion) order
come out exactly as they were produced live.

- An **operator update** (`{$v: 2, diff}`) is rolled forward by re-applying the
  `updateDescription` to the document's current state (`updatedFields` are set,
  `removedFields` unset, `truncatedArrays` shortened) — the inverse of how the
  oplog diff was computed. A **replacement update** simply restores the whole `o`.
- During replay the target's own oplog emission is **suppressed**, because the
  oplog is the *input*, not something to regenerate. (See
  [resume continuity](#change-stream-resume-continuity) for the opt-in exception.)
- Collection options (`capped` / `size` / `max` / `validator` / `viewOn` / …) and
  index / `collMod` (incl. TTL `expireAfterSeconds` retunes) / rename DDL are all
  reconstructed.

### The manifest

Every backup archive embeds a small `pitr-manifest.json` describing the oplog
range it can recover to — floor / head seq, the floor / head timestamps and
wall-clock times, and whether the oplog still reaches genesis (an un-pruned
front). It's advisory (restore reads the oplog directly) but lets tooling report a
backup's recoverable range without opening WiredTiger.

## Snapshot backup

`secantusAdmin.backupArchive` forces a WiredTiger checkpoint and tars the consistent
file set into a single `.tar.gz`. Over the wire it's the
`secantusAdmin.backupArchive` command — taken **against the live server** (a
consistent snapshot off WiredTiger's `backup:` cursor, no downtime):

```python
from pymongo import MongoClient

admin = MongoClient("mongodb://127.0.0.1:27017")["admin"]
admin.command({"secantusAdmin.backupArchive": 1, "outputPath": "/backups/db.tar.gz"})
```

Restore the snapshot by extracting it into a fresh directory — with the
`secantusAdmin.restoreArchive` command — then start a new server on it. A plain snapshot restore lands you at the
backup's checkpoint; for an arbitrary target time, use PITR below.

## Point-in-time recovery

Recovery replays the oplog into a fresh store, stopping at a target timestamp or
wall-clock time. With neither, the whole oplog is replayed ("latest").

### CLI

```bash
# Recover to a cluster timestamp (seconds[,ordinal]); here 2026-06-17T14:30:00Z:
secantusd-mdb restore --source /backups/db.tar.gz \
                     --target-dir /restore/at-1430 \
                     --to-timestamp 1781706600

# Or to a precise cluster timestamp (seconds[,ordinal]):
secantusd-mdb restore --source /path/to/stopped-data-dir \
                     --target-dir /restore/exact \
                     --to-timestamp 1781716542,7

# Without --to-timestamp, the whole oplog is replayed
# ("latest"). Then start a server on the result:
secantusd-mdb --storage-path /restore/at-1430
```

`--source` is a backup `.tar.gz`, a stopped server's data directory, **or** a PITR
archive directory (see [Arbitrary window](pitr-arbitrary-window)
below — auto-detected). `--target-dir` must be a fresh path.

```{note}
The `restore` subcommand is `secantusd-mdb restore`. Its options are
`--source`, `--target-dir`, `--to-timestamp` and `--preserve-oplog`; it takes a
cluster timestamp, not a wall-clock time.
```

### Over the wire

The Rust server has no `secantusAdmin.restoreToTimestamp` command: a restore
is the `secantusd-mdb restore` subcommand run against a stopped source. (The
deprecated Python server exposed the same operation as that wire command; it
is not on the Rust server, and there is no backlog item for it yet.)

### Transactions

Every statement in a multi-document transaction shares one commit timestamp, so
the timestamp cut is always **all-or-nothing** for a transaction — a recovery
point never lands in the middle of one.

## The recovery window

### Live oplog (the simple case)

The simplest restore replays onto an **empty** base, which is exact whenever the
source oplog still reaches genesis — i.e. it hasn't been pruned from the front.
The recovery window is then the **oplog retention window**. Tune it for the
horizon you need:

```bash
secantusd-mdb --oplog-retention-seconds 604800 --oplog-max-entries 5000000   # ~1 week
```

(or the `[oplog]` section of `secantusd.toml`). The rule of thumb: *keep enough
oplog and you can rewind to any point in it.* If the oplog has been pruned past
genesis and no archive is configured, this restore **fails loudly** rather than
silently rebuilding a partial database.

(pitr-arbitrary-window)=
### Arbitrary window: oplog archiving + base snapshots

To recover to a time *before* the live oplog floor — without keeping the entire
oplog live — turn on **oplog archiving** and take periodic **base snapshots** into
the same directory:

```bash
secantusd-mdb --storage-path /data --oplog-archive-dir /pitr-archive
```

With `--oplog-archive-dir` set, the rows `prune_oplog` is about to drop are first
written to durable segment files (`oplog-<start>-<end>.seg`) in that directory.
Take base snapshots on demand with the wire command below:

```python
admin.command({"secantusAdmin.archiveBaseSnapshot": 1, "archiveDir": "/pitr-archive"})
```

Each writes a `base-<headSeq>.tar.gz` into the directory. To recover, point
`restore` at the **archive directory** (the CLI auto-detects it):

```bash
secantusd-mdb restore --source /pitr-archive --target-dir /restore/at-T \
                     --to-timestamp 1781082000      # 2026-06-10T09:00:00Z
```

Restore picks the newest base snapshot at or before the target time, extracts it,
and stitches the archived oplog forward onto it up to the target — so any moment
in the archived history is reachable. If the base snapshots plus segments don't
cover the requested time (a gap), it fails loudly rather than returning a
truncated database.

## Change-stream resume continuity

By default the restored data directory starts a **fresh oplog timeline** — the
replayed history isn't carried into the target, so a change stream on the restored
server resumes only from the restore point forward (this matches `mongorestore`).

Pass `--preserve-oplog` (`secantusd-mdb restore`) to carry the replayed oplog onto the restored
directory **verbatim** — same seq, timestamp, and pre-images. A change stream on
the restored server can then resume from a [resume token](change-streams.md)
minted *before* the restore point, because the rows that token references are
present.

## Quick reference

| Task | CLI | Wire command |
|------|-----|--------------|
| Snapshot backup | — | `secantusAdmin.backupArchive` |
| Extract a snapshot | — | `secantusAdmin.restoreArchive` |
| Restore to a time | `secantusd-mdb restore --to-timestamp` | — |
| Restore "latest" | `secantusd-mdb restore` (no bound) | — |
| Carry oplog for resume | `--preserve-oplog` | — |
| Take a base snapshot | — | `secantusAdmin.archiveBaseSnapshot` |
| Restore from an archive dir | `secantusd-mdb restore --source <dir>` | — |
| Enable oplog archiving | `--oplog-archive-dir DIR` | — |

## Notes & limitations

- Restore is **offline**: it writes a fresh data directory you then start a new
  server on. There is no in-place / hot restore (neither does `mongod`).
- The source must be a **stopped** server's data directory or a backup archive —
  WiredTiger's single-writer lock forbids opening a live one. Take a
  `backupArchive` from the live server instead, then restore from that.
- Base snapshots are taken on demand: the operator runs `archiveBaseSnapshot`
  explicitly.
- See [Change streams](change-streams.md) for the oplog model, [Running in
  production](production.md) for a deployment shape, and
  [Compatibility](compatibility.md) for the broader divergence list.
