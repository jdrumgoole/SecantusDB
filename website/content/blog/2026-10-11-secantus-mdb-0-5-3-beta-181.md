Title: secantus-mdb 0.5.3-beta.181 fixes distinct, singleBatch and tailable cursors
Date: 2026-10-11 23:00:00
Slug: secantus-mdb-0-5-3-beta-181
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.181 makes distinct compare by value, singleBatch honour batchSize, tailable cursors keep their filter, and min and max bound index keys, after a comparison of the cursor commands with mongod.

`distinct` on `secantusd-rs` could return the same number more than once.

```python
db.prices.insert_many([{"p": 1}, {"p": 1.0}, {"p": Decimal128("1")}])
db.prices.distinct("p")
```

`mongod` answers `[1]`. The Rust server answered all three, because it
compared the stored values exactly and those are three different types. It
now compares them as `mongod` does, and returns the values in sort order.

This release is the result of comparing the cursor commands on the server
and on `mongod` 8.2.11: `find` and its options, `getMore`, `killCursors`,
tailable cursors, `count` and `distinct`. There are 85 scripts of commands.
The previous release answered 49 of them differently. This one differs on 5.

**Wrong results that are fixed.**

- `distinct` kept one value for each type, as above. It also did not read a
  dotted key through an array of documents: over
  `{items: [{sku: "a"}, {sku: "b"}]}`, a `distinct` on `items.sku` returned
  nothing for that document.
- `find` with `singleBatch: true` returned every matching document. It now
  returns one batch of `batchSize`.
- A tailable cursor applied its filter and projection to the first batch
  only. Every document inserted afterwards came back, whole, whether it
  matched or not.
- `min` and `max` compared documents. `mongod` compares the keys of the
  hinted index, so a document whose array has one element inside the bounds
  is returned, and so is a document without the field when null is inside
  them. Both were left out.
- `count` ignored a negative `limit`. `mongod` reads `-3` as 3.

**Cursors that outlive their collection.** A cursor whose collection is
dropped or renamed now answers its next `getMore` with code 175 and the
reason, as `mongod` does. After `dropDatabase` the server used to go on
returning the documents it had kept.

**Arguments.** Most malformed arguments to these commands were accepted and
ignored. They are refused now, with `mongod`'s code and message:

- an unknown field on `find`, `getMore`, `killCursors`, `count`, `distinct`,
  `aggregate`, `listIndexes` or `listCollections`;
- a negative `batchSize`, a cursor id that is not an int64, a `getMore`
  naming another collection, `maxTimeMS` on a `getMore` of an ordinary
  cursor;
- `awaitData` without `tailable`, a tailable cursor with a sort or with
  `singleBatch`;
- `min` or `max` with no hint, with an index that does not fit, or with
  `min` not below `max`;
- a collection name that is not a string, or is empty, on any command.

If a test passes a field one of these commands does not know, it will now
fail, as it would against `mongod`.

Two other replies changed. `find` with `batchSize: 0` opens a cursor even
when nothing matches. A tailable `find` on an empty capped collection
returns a closed cursor.

What this release does not do:

- A cursor here is the result as it was when the command ran. On `mongod`
  a `getMore` sees a document inserted or deleted since the cursor opened.
- `listIndexes` returns indexes in name order after `_id_`. `mongod`
  returns them in the order they were built.
- A cursor keeps working after the index it was reading is dropped.
  `mongod` kills it.
- A projection of `{$meta: "recordId"}` or `{$meta: "indexKey"}` adds
  nothing.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.181` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.181)
