Title: secantus-mdb 0.5.3-beta.182 gives time-series collections their buckets and makes _id respect the collation
Date: 2026-10-12 00:30:00
Slug: secantus-mdb-0-5-3-beta-182
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.182 shows a time-series collection's buckets, makes _id unique by the collection's default collation, stops a failed $out from destroying its target, and adds convertToCapped, cloneCollectionAsCapped, compact and reIndex.

Two things in this release could leave a test passing on wrong data.

- **A failed `$out` emptied its target.** The target was dropped before the
  new documents went in. If one of them was refused, by a duplicate `_id`
  or by a unique index the output broke, the target was left empty or
  half-written, and a duplicate `_id` in the output was dropped without an
  error. `$out` now builds the replacement beside the target and swaps it
  in, as `mongod` does, so a refused output leaves the target as it was.
- **`_id` was not unique by the collection's default collation.** With a
  default of `{locale: "en", strength: 2}`, `mongod` refuses `{_id: "A"}`
  once `{_id: "a"}` is there. `secantusd-rs` accepted it, on insert and on
  upsert. It refuses it now, with `mongod`'s error. A collection created
  with a default collation by an earlier release is not protected for the
  documents already in it.

`$merge` had the same blind spot: it matched by a simple comparison
whatever the target's collation, and so inserted a second spelling. It
matches under the target's collation now and refuses to change the `_id`
it matched (66), as `mongod` does.

**Time-series collections.** `mongod` stores a time-series collection as
bucket documents in `system.buckets.<name>` and presents the collection as
a view over them. This server stores the measurements, in the format the
Python server shares, and until now had no buckets to show. It derives
them now, whenever something asks:

- `system.buckets.<name>` and `system.views` can be read, counted, listed
  and used as the base of a view. A bucket has `mongod`'s shape:
  `control.min` and `control.max`, `meta`, and `data` columns that
  `mongod` 8.2.11 unpacks to the same measurements.
- A scan of the collection returns its measurements bucket by bucket, each
  with the time field first and the meta field second, as `mongod` does.
- `validate`, `collStats` and `$collStats` report buckets, and
  `listIndexes` on the buckets shows each index as it lies on them.
- A document that is not a bucket is refused by the bucket schema (121).

The comparison with `mongod` 8.2.11 covers 187 time-series scenarios; the
previous release differed on 24, this one on 6. The buckets are a
derivation, not storage: a bucket cannot be written directly, and `mongod`
places a batch of measurements as a whole where this server places them
one by one, so an insert batch that runs backwards in time can land in
more buckets here. The release notes in `tasks/backlog.md` list the rest.

**Four commands that answered `no such command`.** `convertToCapped` and
`cloneCollectionAsCapped` copy a collection into a capped one, keeping its
options and the newest documents the cap allows, with `mongod`'s checks
and errors. `compact` and `reIndex` answer as `mongod` does: refused on a
replica-set member, which this server is unless started with
`--standalone`, and `bytesFreed: 0` / the index list otherwise.

Smaller changes: a duplicate key on a clustered collection is worded as
`mongod` words it, with `foundValue`; `validate` reports `repairMode`, the
collection's `uuid` and each index's `spec`; `$out` to a capped collection
is refused (17152).

We ran the pymongo, Go and Node driver suites against this code. Each
gave the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.182` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.182)
