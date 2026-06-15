# Change streams

SecantusDB implements MongoDB-compatible change streams against a
single-node, durable oplog. `pymongo`'s `Collection.watch()`,
`Database.watch()`, and `MongoClient.watch()` all work end-to-end —
clients see `insert` / `update` / `delete` events, get back resume
tokens, and can resume by token, `startAfter` token, or
`startAtOperationTime`.

## Watch a collection

```python
from pymongo import MongoClient
from secantus import SecantusDBServer

with SecantusDBServer(port=0) as srv:
    client = MongoClient(srv.uri, directConnection=True)
    db = client["app"]
    db.create_collection("orders")

    cs = db["orders"].watch(max_await_time_ms=2000)
    db["orders"].insert_one({"_id": 1, "total": 9.99})
    event = next(cs)
    assert event["operationType"] == "insert"
    assert event["fullDocument"] == {"_id": 1, "total": 9.99}
    cs.close()
    client.close()
```

`db.watch(...)` watches every collection in the database; `client.watch(...)`
(against the `admin` DB) watches the whole server. Subsequent pipeline
stages (`$match`, `$project`, etc.) after `$changeStream` filter the
events stream as they would on real `mongod`.

## Resume tokens

Every event carries an opaque `_id` resume token. Pass it back to resume
the stream:

```python
event = next(cs)
token = event["_id"]
cs.close()

# Later, possibly in a new process — but only if the SecantusDB server is
# on-disk and survives. With `:memory:` storage, tokens die with the process.
cs2 = db["orders"].watch(resume_after=token)
```

Tokens carry the oplog seq, cluster timestamp, namespace, and
`documentKey._id` in their internal payload. The format is **not
wire-compatible** with real `mongod`'s keystring tokens, so a token
issued by SecantusDB cannot be presented to a real `mongod` and vice
versa — but `pymongo` round-trips them faithfully.

`startAtOperationTime` is honoured too: pass the `Timestamp` from a
previous `hello` reply's `lastWrite.opTime.ts`, or any cluster time
returned in an `aggregate` reply's `operationTime`.

## Full document on update

By default, `update` events carry an `updateDescription` (faithful diff)
but no full document. Ask for the post-image:

```python
cs = db["orders"].watch(full_document="updateLookup")
db["orders"].update_one({"_id": 1}, {"$set": {"total": 19.99}})
event = next(cs)
assert event["fullDocument"] == {"_id": 1, "total": 19.99}
```

`updateLookup` re-fetches the current document at projection time. If
the doc has since been deleted, `fullDocument` is `None` for
`updateLookup` / `whenAvailable`; `required` raises
`ChangeStreamFatalError` (code 280).

## Pre-images via `fullDocumentBeforeChange`

Enable pre-image storage on the collection at creation time (or via
`collMod`):

```python
db.create_collection(
    "audit",
    changeStreamPreAndPostImages={"enabled": True},
)
db["audit"].insert_one({"_id": 1, "n": 1})

cs = db["audit"].watch(full_document_before_change="whenAvailable")
db["audit"].update_one({"_id": 1}, {"$set": {"n": 2}})
db["audit"].delete_one({"_id": 1})

update_ev = next(cs)
delete_ev = next(cs)
assert update_ev["fullDocumentBeforeChange"]["n"] == 1
assert delete_ev["fullDocumentBeforeChange"]["n"] == 2
```

When pre-images are not enabled on the collection, the field is omitted
(`whenAvailable`) or the lookup raises `ChangeStreamFatalError`
(`required`).

## Retention and durability

The oplog is a real WiredTiger table. With on-disk storage
(`SecantusDBServer(storage_path="/path/to/wt-home")`), oplog rows
survive process restart and resume tokens minted before a restart can
be presented after.

Retention defaults: **1 hour** by wall-clock and **100 000 entries** by
count. Whichever cap kicks in first prunes the oldest entries plus
their paired pre-images. Tune via the `Storage` constructor:

```python
from secantus.storage import Storage
from secantus.server import SecantusDBServer

# A retention-hostile config for tests that exercise resume failures.
srv = SecantusDBServer(port=0, storage_path="/tmp/wt-home")
srv.storage.oplog_retention_seconds = 5.0
srv.storage.oplog_max_entries = 1000
```

Pruning is opportunistic (every 1000 emits) and exposed publicly as
`Storage.prune_oplog(now=...)`. There is no background sweeper — same
pattern as TTL indexes.

A resume token whose seq has been pruned surfaces
`ChangeStreamHistoryLost` (code 286), wrapped by `pymongo` as
`OperationFailure` so the standard "resume failure" handling on the
client side works as written.

## Querying the oplog directly

For the same reason real `mongod` does, SecantusDB exposes the oplog
as a queryable collection at `local.oplog.rs`. It's a read-only
synthetic view over the WiredTiger oplog table — `find` / `count` /
`listCollections` route to the persisted oplog rows; writes
(`insert` / `update` / `delete` / `drop` / `createIndexes` / `create`)
are refused with code 13 (Unauthorized), matching mongod.

This is the right surface for debugging a change-stream pipeline,
auditing what just got written, or building dashboards on top of the
operation log. The entries are the mongod oplog shape — `ts`, `op`
(`"i"` / `"u"` / `"d"` / `"c"` / `"n"`), `ns`, `ui` (collection UUID),
`o` (the operation payload), `o2` (the update predicate for `op:"u"`),
`wall` (datetime).

```python
from pymongo import MongoClient

client = MongoClient("mongodb://127.0.0.1:27017/")
oplog = client["local"]["oplog.rs"]

# What just happened, newest first
for entry in oplog.find().sort("ts", -1).limit(10):
    print(entry["ts"], entry["op"], entry["ns"])

# All updates against a specific collection
for entry in oplog.find({"op": "u", "ns": "appdb.users"}):
    print(entry["o2"]["_id"], entry["o"]["diff"])

# Oplog window
oldest = oplog.find().sort("ts", 1).limit(1)[0]["ts"]
newest = oplog.find().sort("ts", -1).limit(1)[0]["ts"]
print(f"oplog covers {oldest} → {newest}")
```

The collection appears in `client.list_database_names()` (under
`local`) and `client.local.list_collection_names()` as long as the
oplog is enabled. Capped-collection metadata (`size`, `max`) reads back
through `listCollections.options` so admin tools that probe the shape
see what they expect.

`enable_oplog=False` (passed to `SecantusDBServer` / `Storage`) hides
the entire surface — no `local` database, no `oplog.rs` collection,
no oplog rows. The default is `True` since change streams depend on
it.

## Invalidation

A change stream ends with a final `invalidate` event when the watched
scope is destroyed:

- Collection-level watch: dropping or renaming the watched collection.
- DB-level watch: `dropDatabase` on the watched DB.

The invalidate event is the last delivery; the next `getMore` returns
cursor id `0`, signalling end-of-stream to the client.

## Limitations

See [`tasks/backlog.md`](https://github.com/jdrumgoole/SecantusDB/blob/main/tasks/backlog.md)
"Change-stream limitations" for the canonical list. Highlights:

- Transactional writes DO carry `txnNumber` / `lsid` on their events
  (all events from one transaction share the commit `clusterTime`).
- `splitLargeChangeStreamEvents` is honoured at the envelope level —
  every event carries `{splitEvent: {fragment:1, of:1}}` when the
  flag is set — but events never actually split (oplog entries cap
  well below 16 MB in our test surrogate workloads).
- No `noop` heartbeats — resume tokens advance only on real ops.
- DDL "expanded" events (`createIndexes` / `dropIndexes`) are
  emitted only when the caller passes `show_expanded_events=True`
  to `coll.watch()` (mongod-faithful default-off). Without the
  opt-in, the v1 spec's stable event set
  (insert / update / delete / replace / drop / dropDatabase /
  rename / invalidate) is what surfaces.
- Resume-token internals are not wire-compatible with real `mongod`
  keystring tokens (round-trip through `pymongo` is fine).
