Title: secantus-mdb 0.5.3-beta.186 stops an update rewriting sub-documents shaped like extended JSON
Date: 2026-10-12 22:00:00
Slug: secantus-mdb-0-5-3-beta-186
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.186 fixes an update that rewrote stored sub-documents shaped like extended JSON, stores new fields in mongod's order, and corrects which array element the positional operator writes.

Upgrade if you run `secantusd-rs`. This release fixes an update that
changed a field it did not name.

- **A stored sub-document shaped like extended JSON was rewritten.** If a
  document held `{a: {$numberLong: "1"}}`, an update of any other field of
  that document stored `{a: 1}`. The same happened to `{$oid: "..."}`,
  `{$date: ...}`, `{$numberInt: "..."}` and the other extended JSON shapes,
  and when the document passed through `aggregate`. An insert stored the
  sub-document correctly, so the damage appeared only later. `mongod` keeps
  the sub-document, and so does this server now.

The cause was the BSON decoder the server used, which reads those shapes as
the values they stand for. Every decode now keeps a sub-document as the
document it is.

We found it by sending 1,295 `update` and `findAndModify` commands to the
server and to `mongod` 8.2.11 and comparing each reply and the documents
left behind. The previous release differed on 213 of the first 1,096. This
one differs on 17.

The same comparison found other writes that stored the wrong document:

- **New fields were stored in the order the update wrote them.** `mongod`
  stores them in the order of their paths, across all the update's
  operators: `{$set: {z: 1, b: 2}, $inc: {a: 1}}` adds `a`, `b`, `z`.
- **`$` could write the wrong array element.** When a query tests two
  arrays, `mongod` has one position for the whole update. `{a: 3, b: 5}`
  with `a.$` writes the element of `a` at the position where `b` matched.
  This server used the position where `a` matched.
- **A pipeline update could change `_id`.** It is refused, as on `mongod`.
- **`{$set: {$x: 1}}` stored a field named `$x`.** It is refused.
- **`$bit` turned a long into an int**, and `$addToSet`, `$pull`,
  `$pullAll` and `$push` with `$sort` stored the wrong array for seven
  argument shapes.
- **A pipeline `$set` of a dotted path** followed the rules of the update
  operator. It now follows the aggregation stage's, in `aggregate` too.
- **An update with no `upsert` created the collection it named.**
- **`findAndModify` read `upsert: 1` as false**, so nothing was inserted.

Some updates that `mongod` refuses were accepted: an upsert whose query
names both `a` and `a.b`, an upsert with `$expr`, a document nested deeper
than 180 levels, and unknown fields on `update` and `findAndModify`. About
sixty malformed operator arguments now get `mongod`'s error where the
server had answered that it did not support the construct.

What this release does not do. A document upserted from a query with
several equalities stores those fields in sorted order, and `mongod` uses
an order we could not derive. `$where`, `$text` and `$$CLUSTER_TIME` in an
update are refused. The message for an unknown hint is worded differently
from `mongod`'s.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.186` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.186)
