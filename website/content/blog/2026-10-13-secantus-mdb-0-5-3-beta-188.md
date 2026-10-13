Title: secantus-mdb 0.5.3-beta.188 makes $merge store what mongod stores
Date: 2026-10-13 09:00:00
Slug: secantus-mdb-0-5-3-beta-188
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.188 fixes $merge, which merged nested documents field by field and stored a different document from mongod, and stops $out and $merge running inside $facet, $lookup and $unionWith.

Upgrade if you run `secantusd-rs` and your pipelines end in `$merge`. Its
default mode stored a different document from `mongod`.

- **`$merge` merged nested documents field by field.** With
  `whenMatched: "merge"`, which is the default, merging `{o: {p: 1}}` into
  a stored `{o: {q: 2}}` gave `{o: {q: 2, p: 1}}`. `mongod` replaces each
  top-level field whole and stores `{o: {p: 1}}`. This server now does the
  same.

`mongod` writes each document of a `$merge` with an ordinary update: a
`$set` of the document for `merge`, a replacement for `replace`, and so on.
The stage here is rebuilt on the server's own `update` command, so it
stores what that command stores. Two things follow that you may notice. A
field name with a dot in it is a path, so `{"a.b": 1}` is stored as
`{a: {b: 1}}`. And new fields are stored in the order `$set` gives them,
which is by name.

We found this by sending 376 `$out` and `$merge` pipelines to the server
and to `mongod` 8.2.11, and comparing each reply and what the source and
target collections held afterwards. The previous release differed on 191
of the first 327. This one differs on 4.

The same comparison found other pipelines that wrote the wrong thing:

- **`$out` and `$merge` ran inside `$facet`, `$lookup` and `$unionWith`,**
  and wrote to their target from there. `mongod` refuses them, and nothing
  is written.
- **One refused document stopped the rest.** `mongod` attempts every write
  and reports the first failure, so the documents after a refused one are
  still written. That holds for a duplicate key and for a document the
  target's validator refuses.
- **A `whenMatched` pipeline could insert a document the target's
  validator refuses.**
- **`$out` wrote to a system collection** and to names no collection can
  have.
- **`whenMatched: "delete"` was accepted.** `mongod` has no such mode.

Three fixes reach beyond these two stages:

- **A field set to `$$variable.path` that leads nowhere was set to
  null.** `mongod` removes the field. This affected `aggregate` and
  pipeline updates.
- **A replacement could change an `_id`** from `"a"` to `"A"` in a
  collection whose collation holds them equal.
- **A duplicate-key error for a whole command** had no `keyPattern` or
  `keyValue`.

What this release does not do. With `cursor: {batchSize: 0}`, `mongod`
does not run `$out` or `$merge` until the first `getMore`; this server
runs it at once. When `$merge` inserts a document and `on` names several
fields, the order of those fields in the stored document can differ.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.188` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.188)
