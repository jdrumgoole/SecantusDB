Title: secantus-mdb 0.5.3-beta.198 and secantus-pg 0.1.0-beta.20
Date: 2026-11-17 22:00:00
Slug: secantus-mdb-0-5-3-beta-198-pg-0-1-0-beta-20
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.198 and secantus-pg 0.1.0-beta.20 ship together: a zlib security fix in both, closer agreement with mongod's diagnostics and refusals, and PostgreSQL's semi joins, anti joins and InitPlans in EXPLAIN.

Both Rust servers have a new release, and both are on PyPI, crates.io and
the GitHub releases page:

```bash
pip install --upgrade SecantusDB
cargo install secantus-mdb --version 0.5.3-beta.198
cargo install secantus-pg --version 0.1.0-beta.20
```

**Upgrade for the security fix.** Both servers link zlib through the
`libz-sys` crate. Version 1.1.31 is covered by
[RUSTSEC-2026-0544](https://rustsec.org/advisories/RUSTSEC-2026-0544);
this release moves to 1.1.33.

## The MongoDB server

Most of the work makes the server report and refuse things the way
`mongod` 8.2 does:

- **Slow-query lines and the profiler.** A `Slow query` line names the
  collection a command works through, the plan a contested read chose and
  how it chose it, and the locks `mongod` reports. Refused commands are
  profiled as `mongod` profiles them, and more queries carry `mongod`'s
  `queryShapeHash`.
- **Views.** `$collStats` of a view answers; `$out` and `$merge` into a
  view are profiled under it; `dropIndexes` of `system.views` answers as
  `mongod` does.
- **Refusals in `mongod`'s order.** A malformed `create`, a nested
  pipeline, a window stage field of the wrong type, a bare `"$"` field
  path, and `getMore`'s arguments are checked and refused in the order
  `mongod` uses. The Atlas Search stages are refused as `mongod` refuses
  them.
- **Explain.** `returnKey` explains its sort-key generator, `$near`
  reports `GEO_NEAR` execution stats, and a time-series read bounded on its
  time field names a clustered scan.
- **New surface.** `$scoreFusion` combines by `"avg"`, `find` returns a
  projection's `{$meta: "sortKey"}`, and `deleteIndexes` runs as
  `dropIndexes`.
- **Behaviour.** An `awaitData` `getMore` on a capped collection now waits
  instead of spinning, and WiredTiger's own startup messages reach the log.

The Windows release now carries a `.zip` alongside the `.tar.gz`.

## The PostgreSQL server

Most of the work brings `EXPLAIN` closer to PostgreSQL's planner:

- **Joins from subqueries.** `IN` and `EXISTS` subqueries are planned as
  PostgreSQL's semi joins (including unique-ified ones), `NOT EXISTS` as a
  hash anti join, and an uncorrelated scalar subquery as an InitPlan.
  Simple subqueries in `FROM` are pulled up into their parent.
- **Estimates and costs.** Joins filtered by expressions or patterns,
  function scans over user functions, sorts on computed keys, set
  operations over joins and aggregates, and a chain of inequality joins
  planned as the bushy tree PostgreSQL keeps.
- **`EXPLAIN VERBOSE`** prints the output lists of nested loops, hash joins,
  sorts and limits.
- **Index sizes.** Hash indexes (overflow pages included), GIN indexes over
  collated text and `btree_gin`, and partitioned tables report their real
  sizes.
- **DDL and errors.** Renaming a column carries its `CHECK` constraints;
  `NOT VALID` checks print as such; a `CHECK` may name its own table; a
  partition's violation names the partition; types made in `pg_temp` live
  in the session's temp schema; and more errors now match PostgreSQL's
  wording and position.

Both servers remain betas: single-node, measured against `mongod` 8.2 and
PostgreSQL 15, with the open gaps listed in the backlog.

[MongoDB server](/rust-db.html) ·
[PostgreSQL server](/rust-pg.html) ·
[PyPI](https://pypi.org/project/SecantusDB/0.5.3b198/) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb/0.5.3-beta.198) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg/0.1.0-beta.20) ·
[secantusdb-v0.5.3-beta.198](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.198) ·
[secantusd-pg-v0.1.0-beta.20](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.20)
