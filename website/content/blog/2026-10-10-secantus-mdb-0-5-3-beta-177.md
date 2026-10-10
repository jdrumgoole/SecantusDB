Title: secantus-mdb 0.5.3-beta.177 stops a partial TTL index deleting documents it does not cover
Date: 2026-10-10 11:30:00
Slug: secantus-mdb-0-5-3-beta-177
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.177 fixes TTL indexes that deleted too much and told nobody, and a bulkWrite command that applied half a malformed batch, each found by running the same commands against mongod 8.2.11.

If you use a TTL index with a `partialFilterExpression` on `secantusd-rs`
0.5.3-beta.176 or earlier, upgrade. The index expired every old document in
the collection, including the ones its filter leaves out.

```python
db.sessions.create_index(
    "last_seen",
    expireAfterSeconds=3600,
    partialFilterExpression={"anonymous": True},
)
```

On `mongod` that index expires anonymous sessions and leaves the rest alone.
The Rust server deleted both. It now deletes what `mongod` deletes.

We found this the way we found the last batch: by sending the same commands
to the server and to `mongod` 8.2.11 and comparing every reply. This time the
commands were TTL indexes (50 scenarios) and the `bulkWrite` command (141).

**TTL indexes.** Three more things were wrong beside the partial filter.

- An expiry was written nowhere. There was no oplog entry, so a change
  stream watching the collection never saw the delete. It sees it now.
- A document whose TTL field is an array of dates did not expire. `mongod`
  expires it once any date in the array has passed.
- `collMod` accepted `expireAfterSeconds` on a compound index, and the server
  then expired documents by that index's first field. `mongod` refuses the
  `collMod`. So does the server now, along with a negative or non-numeric
  value and a TTL on `_id`.

Two things are new for anyone testing expiry. `setParameter` takes
`ttlMonitorSleepSecs`, so a test can make the monitor pass every second
instead of every minute. And `serverStatus` reports `metrics.ttl.passes`, so
the test can wait for a pass instead of sleeping.

```python
client.admin.command("setParameter", 1, ttlMonitorSleepSecs=1)
```

**The `bulkWrite` command.** This is the server command MongoDB 8.0 added,
which a driver's `client.bulk_write()` sends. The server checked each
operation as it reached it, so a batch whose third operation was malformed
had already applied the first two when it failed. `mongod` checks the whole
command first and writes nothing. The server now does the same, with
`mongod`'s error for each case. An operation aimed at a view was accepted;
it now fails with `mongod`'s error 166, and the rest of the batch carries on
or stops according to `ordered`.

**Inserts and updates.** The comparison turned up four bugs in the ordinary
write commands.

- An `_id` that is an array or a regular expression was stored. `mongod`
  refuses both, on insert and on an upsert that would create one.
- An upsert that landed on an `_id` already in use answered
  `E11000 duplicate key error` and nothing else. It now names the collection,
  the index and the key, as every other duplicate key does.
- An update's constants (`c`) and `upsertSupplied` were accepted and ignored.

**Indexes.** `createIndexes` refuses a `partialFilterExpression` built from
an operator a partial index cannot hold, such as `$ne`, `$nin`, `$not` or
`$regex`. It used to build the index.

What this release does not do: an index `collation` is still not checked or
filled in, `listIndexes` still lists by name where `mongod` lists by creation
order, and `text` and `hashed` indexes are still refused.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the day before, test for test.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.177` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.177)
