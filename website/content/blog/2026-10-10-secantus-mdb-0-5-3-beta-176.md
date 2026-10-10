Title: secantus-mdb 0.5.3-beta.176 stops dropIndexes from dropping the _id index
Date: 2026-10-10 09:00:00
Slug: secantus-mdb-0-5-3-beta-176
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.176 and 0.5.3-beta.175 fix a run of wrong answers on capped collections, views, document validation and indexes, each found by running the same commands against mongod 8.2.11.

If you run `secantusd-rs` 0.5.3-beta.174 or earlier, upgrade. The two releases
since then fix places where the server answered without an error and the
answer was wrong. We found them by sending the same commands to the server
and to `mongod` 8.2.11 and comparing every reply.

The one to know about first: `dropIndexes` with the key `{_id: 1}` dropped
the `_id` index.

```python
db.command("dropIndexes", "orders", index={"_id": 1})
```

`mongod` refuses that with `cannot drop _id index`, by name, by key or in a
list of names. So does 0.5.3-beta.176.

The rest, by area. Each line is something that used to succeed silently.

**Inserts and validation** (0.5.3-beta.176). When one document in an ordered
`insert_many` failed the collection's validator, none of the batch was
inserted. `mongod` inserts the documents before the failing one and stops
there, and so does the server now. `create` and `collMod` refuse a
`validationLevel` or `validationAction` they do not know, where they used to
store it and then act as if it were the default.

**Indexes** (0.5.3-beta.176). `createIndexes` checks every spec before it
builds anything. It used to accept a key value of `0`, an empty field name, a
negative TTL, an unknown option and nine other invalid specs, and build an
index from each. `unique: 1` builds a unique index; a number was read as "not
unique". A hidden index is no longer used by queries, sorts or hints; the
server recorded `hidden` and went on using the index.

**Geo** (0.5.3-beta.176). `$geoWithin` with `$box`, `$polygon` or a GeoJSON
polygon left out any point lying exactly on the boundary. `mongod` includes
those points.

**Capped collections** (0.5.3-beta.175). One `insert_many` could take a capped
collection past its cap: five documents into a `max: 3` collection left five.
Upserts were never held to the cap at all. `max: 0`, which means "no limit",
was read as a limit of zero, so every insert evicted everything except the
newest document.

**Views** (0.5.3-beta.175). An `insert`, `update` or `delete` aimed at a view
was acknowledged. `distinct` on a view returned nothing, `$lookup` from a view
matched nothing, and `collMod` of a view's pipeline answered `ok` and changed
nothing.

**Aggregation** (0.5.3-beta.175). `$count` over no documents returned
`{n: 0}`. `mongod` returns no document, which matters inside a `$lookup` or
`$facet` that counts no match.

What these releases do not do: `convertToCapped` is still not implemented,
there is no `system.views` collection, and `text` and `hashed` indexes are
still refused.

We ran the thirteen driver test suites against the 0.5.3-beta.176 code. Every
one gave the same result as the day before, test for test. None of these bugs
was in a path the drivers' own suites exercise, which is why comparing against
`mongod` directly found them and the suites had not.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.176` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.176)
