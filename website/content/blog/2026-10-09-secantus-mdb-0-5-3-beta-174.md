Title: secantus-mdb 0.5.3-beta.174 restores natural-order sorts on find
Date: 2026-10-09 14:00:00
Slug: secantus-mdb-0-5-3-beta-174
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.174 fixes find().sort("$natural", ...), which 0.5.3-beta.173 refused with an error, and findAndModify with a $natural sort, which acted on the wrong document.

If you run `secantusd-rs` 0.5.3-beta.173, upgrade. That release refused every
`find` sorted in natural order:

```python
coll.find().sort("$natural", pymongo.DESCENDING)
```

The server answered `16410 FieldPath field names may not start with '$'`. A
`mongod` returns the documents in storage order, newest first. Reading the last
entry of a capped collection or of the oplog this way is common, and it failed
outright.

The cause was a sort validator shared between `find` and the aggregation
`$sort` stage. The stage does read `$natural` as a field path and rejects it;
`find` does not, and the validator applied the stage's rule to both.

A second fault made no noise. `findAndModify` with
`sort: {$natural: -1}` passed the sort down as a field named `$natural`, which
no document has, so the command acted on the first document in the collection
where `mongod` acts on the last. A `findOneAndDelete` meant to remove the
newest document removed the oldest.

In beta.174 both commands read `{$natural: 1}` and `{$natural: -1}` as storage
order, forwards or backwards, and it composes with a filter, `skip` and
`limit`. `explain` reports a collection scan with its direction. The rules
around it are `mongod`'s: the value must be exactly 1 or -1, `$natural` must be
the only key, a `$natural` hint must point the same way as the sort, and an
index hint is refused. We ran the same 38 commands against `mongod` 8.2.11 and
this build and the answers match.

The fault was found by re-running the driver test suites against both servers.
pymongo's own suite reads the oplog with a natural-order sort, and that one
test failed on the Rust server in the sync and async runs.

`cargo install secantus-mdb --version 0.5.3-beta.174` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.174)
