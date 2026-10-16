Title: secantus-mdb 0.5.3-beta.196 plans queries as mongod does
Date: 2026-10-16 12:00:00
Slug: secantus-mdb-0-5-3-beta-196
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.196 runs mongod's trial of candidate plans, so explain and the order documents come back in follow mongod's choice of index; numbers compare by their exact value; and the server gains a profiler, wire compression and real serverStatus counters.

Upgrade if your tests depend on which index a query reads, on the order a
query returns documents in, or on how a double compares with a long or a
decimal.

**Read this before you upgrade.** The first time this release opens a
store, it rewrites how numbers are keyed in every index, and it rebuilds
collated and geo indexes that an older build made. After that, do not open
the store with an older build. An older build keys a new numeric `_id` the
old way, and nothing would notice the store had become inconsistent.

## Which index a query reads

When more than one index could serve a query, `mongod` tries the candidate
plans against each other and keeps the one that finishes first. This
server used to pick from a fixed order of preference instead. It now runs
`mongod`'s trial: the same round-robin counting of work, the same plan
scores, and index intersections where `mongod` builds them. The winner
decides two things:

- **What `explain` reports**, including `mongod`'s `rejectedPlans`.
- **The order documents come back in.** The same choice decides which
  document a `limit: 1` delete, or an update without `multi`, changes.

We measured this by sending the same queries to this server and to
`mongod` 8.2.11 and comparing the replies:

| probe | replies | different before | different now |
| --- | ---: | ---: | ---: |
| order of `find` results | 6,531 | 97 | 77 |
| `explain` | 791 | 72 | 35 |

Of the 77 order differences, 62 are ties under a sort with a `limit`.
`mongod` breaks those ties through its C++ standard library's heap, so a
Linux `mongod` may break them differently from the macOS one we measured.
The other 15 are the error message for a hint that names no index.

**The trial has a cost.** A query that two indexes could serve now costs
about five times as much on a 2,000-document collection. `mongod` keeps a
plan cache, so it runs the trial once per query shape. This server does
not have that cache yet.

**Some queries got faster.** The server now reads an index for `$ne`,
`$nin`, `$not`, `$exists`, `$type`, `$mod`, regexes and `$elemMatch`,
where it used to scan the whole collection. `{a: {$ne: 5}}` over an index
on `a` takes 0.7 times as long as it did, and a regex with a fixed prefix
a hundredth.

## Numbers compare by their exact value

`mongod` compares a double with a long or a decimal by the double's exact
binary value. This server compared the shortest decimal that prints the
double, which made two kinds of query wrong:

- `find({a: 0.1})` matched `Decimal128("0.1")`. `mongod` does not match
  it, because the double `0.1` is not exactly one tenth.
- With an index, every number above about 1e127, or below about 1e-128 in
  magnitude, shared one key. So `{a: {$gt: 1e200}}` missed `3e250` once
  the field was indexed.

Both now match `mongod`. A run of 824 query shapes, each sent with and
without an index, had 40 shapes return a different set of documents from
`mongod` before this change, and none after it. `$all: [null]` now matches
a missing field. `$mod` no longer matches a double too large to fit a long.

## Geo results in `mongod`'s order

`$geoWithin`, `$geoIntersects`, `$near` and `$geoNear` return their results
in the order `mongod` 8.2 does. To get there, the server follows `mongod`'s
own S2 and geohash code: the cells each document is indexed under, and the
rings a near query searches outward through. `2d` index options, the
errors from building a geo index, and a `$near` centre outside the index's
range all answer as `mongod` does.

## Commands for operators

- **A database profiler.** `profile` levels 1 and 2 write each operation to
  a capped `system.profile` collection, in `mongod`'s format.
- **Wire compression.** A driver configured with `compressors=snappy`,
  `zstd` or `zlib` now compresses its traffic, as it does against
  `mongod`.
- **Real `serverStatus` counters.** The `opLatencies`, `transactions`,
  `wiredTiger` and `metrics` sections, among others, now report real
  counts. Each counter was checked by running the same operations against
  `mongod` and comparing what changed. `mongostat` now fills in its memory,
  cache and queue columns.
- **`$listCatalog` and `notablescan`.** `$indexStats` now counts each time
  a query reads an index. `setParameter notablescan` refuses a query that
  would scan the whole collection, as `mongod` does.
- **One-member replica set.** The commands a tool sends to a one-member
  replica set answer as they do on `mongod`.

## Writes

- **A retried `bulkWrite` no longer runs twice.** Drivers retry a write
  when the connection drops before the reply. A retried `$inc` used to be
  applied twice. The operations that already ran are now replayed from
  their results instead.
- **Two transactions that create the same collection** behave as on
  `mongod`: whichever commits second fails with 112.
- **A clustered collection's `expireAfterSeconds`** now expires documents.
- **A cursor reading a dropped index** ends with `mongod`'s 175.

## Throughput

On three DigitalOcean droplets (one server, two clients), with 8 KiB
incompressible documents and 70% inserts, this release did 10,900
operations a second against `mongod` 8.0.34's 16,886: 0.65 times. We had
published 0.72 to 0.76 times against `mongod` 8.0.32. To see whether this
release lost ground, we ran the previous release, beta.195, against the same
`mongod` 8.0.34. It measured 0.60 times. So this release costs nothing
measurable on this workload; `mongod` 8.0.34 is the faster reference.
[The benchmark page](https://secantusdb.com/docs/benchmark.html) has every
run.

We ran the thirteen driver test suites against this release's code. No
suite lost a test: pymongo passes 99.5%, and the Java, Kotlin, Rust, C++,
C# and PHP extension drivers pass every test they run.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.196` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.196)
