Title: secantus-mdb 0.5.3-beta.184 makes explain follow mongod's planner
Date: 2026-10-12 12:00:00
Slug: secantus-mdb-0-5-3-beta-184
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.184 reports the plan mongod reports from explain, with index bounds and the stages of count, distinct and the write commands, and refuses an unknown query operator on an empty collection.

If your tests run a query against an empty collection on `secantusd-rs`,
upgrade. One thing in this release could leave a test passing on a query
`mongod` refuses.

- **An unknown query operator was accepted when there was nothing to
  match.** `{a: {$nope: 1}}` was refused only when the server tested it
  against a document. Over an empty collection, or one that did not exist,
  `count`, `distinct`, `aggregate`, `update`, `delete` and `findAndModify`
  answered as if the filter were valid. `mongod` refuses it when it parses
  the filter (`unknown operator: $nope`), and so does this server now.
- **`count` and `delete` by `_id` on a collection that was never written
  could fail** with an internal storage error. They find nothing, as
  `mongod` does.

We found both while comparing `explain` with `mongod` 8.2.11, which is what
most of this release is.

**`explain` reports the plan `mongod` reports.** We had left most of
`explain` out, on the view that it describes `mongod`'s cost model. That
was wrong for most of it. When only one index can serve a query, the plan
follows fixed rules, and those can be reproduced. We sent 786 `explain`
commands to both servers and compared every reply. The previous release
differed on 771. This one differs on 72.

- **Index bounds.** Every index scan carries `indexBounds` in `mongod`'s
  notation: `(1, 4)` for a range, one interval per value of an `$in`, the
  complement for `$ne` and `$nin`, the string range of a prefix regex, and
  high-to-low for a descending field.
- **What the fetch rechecks.** A `FETCH` carries a `filter` only for what
  the bounds do not decide, so a query the index answers outright is told
  apart from one that rereads documents.
- **Sort, paging and projection.** A sort the index serves has no `SORT`
  stage; one on a few points of a leading field is a `SORT_MERGE`; a
  projection the index holds is `PROJECTION_COVERED` with no `FETCH`.
- **Point lookups** through `_id` or a unique index are `EXPRESS_IXSCAN`.
- **The other commands.** `count` is `COUNT_SCAN` or
  `RECORD_STORE_FAST_COUNT`, `distinct` is `DISTINCT_SCAN`, and the writes
  are `UPDATE`, `DELETE`, `BATCHED_DELETE`, `EXPRESS_UPDATE` and
  `EXPRESS_DELETE`. An explained write changes nothing.
- **`executionStats`** counts the keys and documents that plan examines,
  stage by stage. A `LIMIT` stops its input early, as on `mongod`.
- **Argument checks** are `mongod`'s: the verbosity, unknown fields, a
  write concern, commands that cannot be explained.

What `explain` does not do. `rejectedPlans` is always empty. Where several
indexes could serve one query, `mongod` tries them and keeps the fastest;
this server picks by a fixed order of preference, which chose the same
index as `mongod` in every case we measured. An `aggregate` whose `$group`
or `$lookup` `mongod` runs inside its query engine is reported as a cursor
followed by the stages. And `explain` describes the plan `mongod` runs: for
some queries this server reads the documents by a collection scan where the
plan shows an index. The results are the same.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.184` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.184)
