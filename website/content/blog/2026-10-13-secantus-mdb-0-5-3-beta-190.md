Title: secantus-mdb 0.5.3-beta.190 answers geo queries as mongod does
Date: 2026-10-13 20:00:00
Slug: secantus-mdb-0-5-3-beta-190
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.190 tests GeoJSON shapes on the sphere, returns $near results nearest first, and refuses what a geo index cannot hold, as mongod does.

Upgrade if you run `secantusd-rs` and your tests use geo queries. Several
of them returned different documents from `mongod`.

- **GeoJSON shapes were tested as if the map were flat.** `mongod` tests
  `$geometry` on the sphere, where an edge is the shortest path between
  its ends. The point `[1, 1]` is not on the line from `[0, 0]` to
  `[2, 2]`, so `$geoIntersects` does not return that line; this server
  did. A polygon drawn across the 180th meridian is the small one, and
  this server read it as the one that goes the long way round.
- **`$box`, `$polygon` and `$center` returned lines and polygons.** In
  `mongod` they return points only.
- **`$near` did not sort.** It returned matching documents in the order
  they were stored. `mongod` returns the nearest first.
- **A field holding several locations was not matched.** A value such as
  `[[1, 1], [30, 30]]` matched no query, and was not found through a `2d`
  index.

Three things `mongod` refuses are refused here now:

- **`$near` with no geo index** on the field, and `$near` inside `$or`,
  `$nor`, `$not` or an aggregation `$match`.
- **A value a geo index cannot hold.** A `2dsphere` index used to accept
  `loc: "x"` and a point at longitude 200, and a `2d` index used to accept
  GeoJSON. The insert, the update and the index build are refused, with
  the document in the message.
- **`$geoNear` after another stage**, and with the removed `limit` and
  `num` options.

`$geoNear` also changed its units. With a GeoJSON `near` it measures in
metres. With a legacy point and `spherical: true` it measures in radians;
this server gave metres. It no longer needs `distanceField`.

We found these by sending 1,278 geo commands to the server and to `mongod`
8.2.11 and comparing each reply. The previous release differed on 726.
This one differs on 137.

What this release does not do. About 100 of the 137 are order: through a
geo index `mongod` returns documents in the order of its index, and this
server returns them in the order they were stored. `$near` is sorted by
distance, but two documents at the same distance can come back the other
way round. Some `2d` index options are accepted that `mongod` refuses.

Two things to know when you upgrade:

- A geo index built by an earlier version has no entry for a field that
  holds several locations. Drop the index and build it again.
- A `$geoNear` with `maxDistance` reads every document in the collection.
  It used to read the index.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.190` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.190)
