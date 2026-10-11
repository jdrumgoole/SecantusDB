Title: secantus-mdb 0.5.3-beta.179 stops $out dropping a collection's validator, and checks time-series collections
Date: 2026-10-11 14:00:00
Slug: secantus-mdb-0-5-3-beta-179
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.179 makes $out keep its target's validator and indexes, and gives time-series collections the rules mongod applies: a checked definition, a date in every measurement, and meta-only updates.

If a pipeline on `secantusd-rs` ends in `$out` and the target collection has
a validator or indexes, upgrade. The first `$out` removed both.

```python
db.create_collection("report", validator={"total": {"$gte": 0}})
db.report.create_index("day")

db.orders.aggregate([..., {"$out": "report"}])
```

`mongod` replaces the documents in `report` and keeps its validator and its
index. The Rust server dropped `report` and created it again with neither. A
second `$out` could then write a negative `total`, and nothing refused it.
`$out` now keeps the target's options and indexes.

We found it while measuring something else. This release is the result of
comparing time-series collections on the server and on `mongod` 8.2.11: 187
scenarios, each on a fresh database. 139 answers differed. 24 do now.

**What was wrong.** A time-series collection on the Rust server was an
ordinary collection with a label on it.

- `create` accepted any `timeseries` document: no `timeField`, a
  `granularity` of `"days"`, a `metaField` equal to the `timeField`.
- An insert needed no date. A measurement with the time field missing, or
  holding a string, was stored.
- An update was either allowed or refused with one message, whatever the
  reason.

**What it does now.**

- `create` checks the definition and stores it with its defaults, so
  `listCollections` shows `granularity: "seconds"` and
  `bucketMaxSpanSeconds: 3600` as `mongod` does. It refuses `timeseries`
  beside `capped`, `validator`, `viewOn` or `clusteredIndex`.
- An insert refuses a document whose time field is missing or is not a date.
- Updates follow `mongod`'s rules, and each refusal says which rule was
  broken. An update must be `multi`, must not upsert, must use update
  operators, and may query and change the meta field only. Deletes are not
  restricted.
- `findAndModify` takes a `remove` with no sort and refuses everything else.
- A collection with a meta field is created with an index on the meta and
  time fields, and lists no `_id` index. Unique and text indexes are refused.
  A TTL index needs a partial filter on the meta field.
- `collMod` can widen the granularity and set or remove `expireAfterSeconds`.
  It cannot narrow the granularity or add a validator.
- A time-series collection cannot be renamed, watched by a change stream or
  written to by `$merge`.

One other reply changed. `dropDatabase` answered
`{dropped: "<name>", ok: 1}`. A `mongod` running as a replica-set member
answers `{ok: 1}`, and the server presents itself as one, so it now does the
same.

What this release does not do: `mongod` stores a time-series collection as
buckets in a second collection, `system.buckets.<name>`, and this server
stores the measurements as ordinary documents. So there is no buckets
collection to list or read. Measurements come back in the order they were
inserted, where `mongod` returns them a bucket at a time. `collStats` has no
`timeseries` section. Most of the 24 remaining differences are these.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.179` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.179)
