Title: secantusd-mdb 0.5.3-beta.197 and secantusd-pg 0.1.0-beta.19: new names, and pip install
Date: 2026-11-16 21:00:00
Slug: secantus-mdb-0-5-3-beta-197
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: The Rust MongoDB server's binary is now secantusd-mdb, both Rust servers install with pip install SecantusDB, and a month of work lands in secantus-mdb 0.5.3-beta.197 and secantus-pg 0.1.0-beta.19.

Two releases ship together: the MongoDB server as **0.5.3-beta.197** and
the PostgreSQL server as **0.1.0-beta.19**. They are the first releases in
a month, and the first you can install with `pip`.

## The MongoDB server's binary is now `secantusd-mdb`

**Read this before you upgrade.** The Rust MongoDB server's binary was
called `secantusd-rs`. It is now `secantusd-mdb`, so the two servers are
named for the protocol they speak: `secantusd-mdb` and `secantusd-pg`.
The archives on the release, the crate's binary and the documentation all
use the new name. There is no alias, so a script that runs `secantusd-rs`
needs the new name. The crate is still `secantus-mdb`, and the storage
format has not changed.

## `pip install SecantusDB`

The `SecantusDB` package on PyPI now carries both Rust servers as
prebuilt binaries, for Linux x86_64 and macOS arm64:

```bash
pip install SecantusDB
```

```python
from secantusdb import MongoServer, PostgresServer

with MongoServer() as mdb:
    print(mdb.uri)      # mongodb://127.0.0.1:<port>/
with PostgresServer() as pg:
    print(pg.dsn)       # postgresql://...
```

Both binaries are also on your `PATH` as `secantusd-mdb` and
`secantusd-pg`. The package version follows the MongoDB server
(`0.5.3b197`); it bundles the newest PostgreSQL server release, here
0.1.0-beta.19. Nothing is compiled when you install, and there is no
Python server in the package: the old Python servers are no longer
published, and their releases on PyPI are yanked.

## What changed in the MongoDB server

About 320 changes since 0.5.3-beta.196, nearly all of them closing a
difference from `mongod` 8.2 found by sending the same operation to both.
The larger ones:

- **Collation with `mongod`'s own ICU.** Strings under a collation now
  compare with ICU 57.1, the version `mongod` 8.2 links, built from
  `mongod`'s own copy and data. Sort order and matching under a collation
  follow `mongod` character for character.
- **A real lock manager.** `serverStatus.locks` reports real counts, and
  DDL waits for the transactions that hold its collection, as on `mongod`.
- **A journaled write survives a crash.** A write acknowledged with
  `j: true` is on disk when the server is killed.
- **`$score`, `$rankFusion` and `$scoreFusion`** for hybrid ranking.
- **`explain` and the plan cache.** Much of this month went into matching
  `mongod`'s explain output: SBE plans under `queryFramework: "sbe"`,
  work counts in `executionStats`, the pipeline rewrites `mongod`'s
  optimiser makes, `COUNT_SCAN`, `DISTINCT_SCAN` and geo stages.
- **Diagnostics.** The slow-query log and the profiler break down where an
  operation's time went, and the server writes FTDC samples to
  `diagnostic.data`.
- **Time-series collections** read a bucket at a time behind a projection,
  and their indexes list as `mongod` keeps them.

## What changed in the PostgreSQL server

0.1.0-beta.19 follows 0.1.0-beta.17, about 520 changes later.
(0.1.0-beta.18 was tagged but never published: its Linux build was
refused because it needed the build machine's C++ runtime. Both servers'
Linux binaries now link it statically.) The larger changes:

- **`SERIALIZABLE` is serializable**, using PostgreSQL's own
  serializable snapshot isolation, including over partial, hash, GIN and
  GiST indexes.
- **Indexes have PostgreSQL's pages.** B-tree, GIN (`jsonb`, `tsvector`)
  and hash indexes keep PostgreSQL's page layout, so their sizes, dead
  entries and splits behave as PostgreSQL's do.
- **The planner** searches join orders over any tree of inner, left and
  full joins, runs parameterised joins, and splits a hash join too large
  for `work_mem` into batches.
- **Text search**: configurations of your own, and changes to the
  built-in ones. **Collation**: all 907 of PostgreSQL 15's ICU
  collations.
- **`COPY`** streams its rows, checks them and names the line an error
  is on; long commands report their progress.
- **The catalogs**: `pg_rewrite` holds PostgreSQL's rule trees, more of
  `pg_depend` is filled in, and the `information_schema` views run three
  to four times faster.

Both servers are single-node and in beta.

The crates for these versions are not on crates.io yet: this release adds
a new crate, `secantus-icu`, which crates.io needs published by hand once
before our release workflow can publish it. Until then, use `pip` or the
binaries below.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[SecantusDB on PyPI](https://pypi.org/project/SecantusDB/) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.197) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.19)
