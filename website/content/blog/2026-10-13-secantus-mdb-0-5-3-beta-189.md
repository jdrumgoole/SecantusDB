Title: secantus-mdb 0.5.3-beta.189 returns documents in the order mongod does
Date: 2026-10-13 14:00:00
Slug: secantus-mdb-0-5-3-beta-189
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.189 returns documents in the order mongod reads them, so a delete or update of one document changes the document mongod would, and fixes a range query over an array index that left a document out.

Upgrade if you run `secantusd-rs`. This release fixes a query that left a
document out, and changes which document some writes pick.

- **A range over an array index dropped a document.** With an index on
  `a`, `{a: {$gt: 1, $lt: 9}}` did not return `{a: [1, 9]}`. One element is
  over 1 and another is under 9, so the document matches. The server read
  only the index keys between the two bounds, and neither 1 nor 9 is
  between them.
- **`{"p.q": null}` over an index on `p.q` dropped a document** whose
  array `p` has an element with no `q`.

A query with no sort has no promised order, but a client still sees one:
the order of the index the server read. That order also decides which
document a `limit: 1` delete removes, which one an update without `multi`
changes, and which one `findAndModify` returns. This server read through
its own index router and returned its own order, so the same write could
change a different document from `mongod`'s.

It now takes the order from the plan `mongod` runs. For example, with an
index on `a`, `{a: {$ne: 5}}` comes back in the index's order, as it does
on `mongod`. It used to come back in insertion order.

We found this by sending 6,531 commands to the server and to `mongod`
8.2.11, and comparing the `_id`s each read returned, in order, and what
each write of one document left behind. The previous release differed on
1,189. This one differs on 97.

What else changed:

- **`update` and `findAndModify` read their `hint`.** It names the index
  that is read, and so the document that comes first.
- **A sort by a field inside an array of documents** ranks a document as
  null when one of its elements lacks the field. It was ranked by the
  other elements.
- **A sort keeps equal documents in the order `mongod` leaves them.** The
  sort itself is unchanged.

What this release does not do. Under a sort with a limit, `mongod` leaves
equal documents in the order of an internal heap, and this server leaves
them in the order they arrived; 62 of the 97 are that. When several
indexes could serve a query's filter, `mongod` tries them and this server
uses a fixed preference.

Some reads cost more. A read through the index the server already used, a
collection scan and a read by `_id` take the same time as before. A query
such as `{a: {$ne: 5}}` over an index on `a` takes about three times as
long on 20,000 documents, because its results are now put in the index's
order.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.189` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.189)
