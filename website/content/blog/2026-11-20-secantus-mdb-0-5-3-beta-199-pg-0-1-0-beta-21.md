Title: secantus-mdb 0.5.3-beta.199 and secantus-pg 0.1.0-beta.21
Date: 2026-11-20 22:00:00
Slug: secantus-mdb-0-5-3-beta-199-pg-0-1-0-beta-21
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.199 and secantus-pg 0.1.0-beta.21 ship together: stores now use mongod's on-disk layout (a format change), the MongoDB server runs inside your Python process, and it installs as a background service.

Both Rust servers have a new release, on PyPI, crates.io and the GitHub
releases page:

```bash
pip install --upgrade SecantusDB
cargo install secantus-mdb --version 0.5.3-beta.199
cargo install secantus-pg --version 0.1.0-beta.21
```

**This release changes the on-disk format.** A store written by an earlier
release is refused when it is opened, with a message saying what to do: dump
it with the release that wrote it (`mongodump`, or `pg_dump` for the
PostgreSQL server) and restore it into a new store. There is no in-place
migration.

## mongod's storage layout

The format change is the start of moving the stores to the layout `mongod`
itself writes, so that one day a `mongod` can open a SecantusDB store and
SecantusDB can open a `mongod` one:

- WiredTiger is built as `mongod` builds it, compresses with snappy, and
  keeps its log in `journal/` beside `mongod`'s `storage.bson`.
- Every collection is a table of its own (`collection-<UUID>`), listed in
  `_mdb_catalog` in `mongod`'s format. Dropping a collection drops its table;
  renaming one moves it.
- Every index, `_id_` included, is a table of its own (`index-<UUID>`), keyed
  by `mongod`'s KeyString and TypeBits, byte for byte. Unique indexes check
  for duplicates the way `mongod` does.
- The oplog is `local.oplog.rs`: one capped table keyed by timestamp, which a
  tailable cursor can follow. A new `kill -9` test kills both servers fifty
  times under a write burst and checks that no acknowledged write is lost.

Opening a store is also faster: on an M2 Pro a cold start went from 1,432 to
1,032 ms and a restart from 698 to 565 ms.

## The MongoDB server

- **In-process from Python.** `MongoServer()` from `pip install SecantusDB`
  now runs the server inside your Python process, as SQLite does, still on a
  real loopback port. `MongoServer(embedded=False)` keeps the child process.
- **A background service.** `secantusd-mdb service install` runs the server
  at login under launchd (macOS) or a systemd user unit (Linux);
  `service status` and `service uninstall` manage it. The server also takes
  `mongod`'s `--dbpath` and `--bind_ip`.
- **SBE `$group` spills** to a spill engine under a budget you can set, and
  a spilled `$median` / `$percentile` uses `mongod`'s t-digest.
- **Closer to `mongod`'s diagnostics**: lock counts, interrupt checks,
  examined counts in the profiler, startup log lines, and the sort key
  through `$unionWith`, `$facet` and `$densify`.

## The PostgreSQL server

- **`bpchar` keys** ignore trailing blanks in unique indexes, `ON CONFLICT`
  and index scans, as PostgreSQL does.
- **The planner** prices more shapes: `IN` subqueries planned as joins, merge
  joins on expression keys, joins over tables never analysed, and limits and
  aggregates above InitPlans. Regex selectivity estimates cover groups,
  anchors and bounded repeats.
- **`EXPLAIN VERBOSE`** prints chains of three or more joins, including
  `LEFT` and `FULL` joins, and their filters.
- **The catalog**: `pg_get_viewdef` prints the `information_schema` and
  statistics views, built-in objects carry initdb's descriptions, and the
  `record_image` operators cover arrays, ranges, multiranges and composites.

Both servers remain betas: single-node, measured against `mongod` 8.2 and
PostgreSQL 15, with the open gaps listed in the backlog.

[MongoDB server](/rust-db.html) ·
[PostgreSQL server](/rust-pg.html) ·
[PyPI](https://pypi.org/project/SecantusDB/0.5.3b199/) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb/0.5.3-beta.199) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg/0.1.0-beta.21) ·
[secantusdb-v0.5.3-beta.199](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.199) ·
[secantusd-pg-v0.1.0-beta.21](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.21)
