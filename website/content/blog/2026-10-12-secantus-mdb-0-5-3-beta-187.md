Title: secantus-mdb 0.5.3-beta.187 refuses a duplicate document _id and keeps a document a $merge could not replace
Date: 2026-10-12 23:30:00
Slug: secantus-mdb-0-5-3-beta-187
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.187 fixes two bugs that left the wrong documents in a collection: a duplicate _id written as a different number type, and a $merge replacement that deleted the document it could not replace.

Upgrade if you run `secantusd-rs`. This release fixes two bugs that left
the wrong documents in a collection.

- **Two documents could share one `_id`.** After `{_id: {a: 1}}`, the
  server accepted `{_id: {a: 1.0}}` and `{_id: {a: NumberLong(1)}}`.
  `mongod` compares the values and refuses both as duplicates. This
  affected an `_id` that is a document; a number or a string was already
  compared by value.
- **A `$merge` could lose a document.** With `whenMatched: "replace"`, the
  server deleted the matched document and then inserted the new one. If a
  unique index refused the new one, neither was left. `mongod` keeps the
  original, and so does this server now. A replaced document also keeps
  its place in the collection, where it used to move to the end.

The fix for the first does not change anything already on disk. A store
written by an earlier version opens and reads as before. The first time
this version opens a store it records the `_id` of every document already
there, so a duplicate of an older document is refused too. That happens
once and reads only the documents whose `_id` is a document. If an earlier
version already stored two documents with one `_id`, both stay.

We found both by sending 905 `insert`, `delete`, `count` and `distinct`
commands to the server and to `mongod` 8.2.11, and comparing each reply
and the documents left behind. The previous release differed on 162. This
one differs on 32.

The same comparison found other writes that touched the wrong documents:

- **A delete statement with no `limit` removed every match.** `mongod`
  refuses the command, and nothing is removed.
- **`delete` ran the statements ahead of one it then refused.** It now
  checks every statement first.
- **A `limit: 1` delete ignored its `hint`.** With `{$natural: -1}`,
  `mongod` removes the newest match. This server removed whichever one it
  met first.
- **`find` with `$in` over an index returned documents in the order of the
  list.** `mongod` returns them in the index's order. A `limit: 1` delete
  with the same query removed a different document.
- **`insert` stored documents `mongod` refuses:** a `$`-prefixed field
  inside an `_id` below its top level, and a document nested deeper than
  180 levels.

Some replies changed too. A duplicate-key error printed a date, a
sub-document, binary data or a timestamp as Rust debug text; it prints
them as `mongod` does. `insert`, `delete`, `count` and `distinct` refuse
fields they do not take. Forty commands that take no `writeConcern`
refuse one.

What this release does not do. `$where` is not run. The message for a
hint that names no index is worded differently from `mongod`'s. When a
document repeats the keys of two unique indexes, the index named in the
error can differ.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.187` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.187)
