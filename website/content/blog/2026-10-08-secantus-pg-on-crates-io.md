Title: The Rust PostgreSQL server is on crates.io
Date: 2026-10-08 09:00:00
Slug: secantus-pg-on-crates-io
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.3 is the PostgreSQL server's first crates.io release, alongside secantus-mdb 0.5.3-beta.173.

`cargo install secantus-pg --version 0.1.0-beta.3` now builds `secantusd-pg`,
the Rust PostgreSQL server, from crates.io. Until this release the choices were
a prebuilt archive for Linux x86_64 or macOS arm64, or a clone of the
repository with its WiredTiger submodule. The crate builds WiredTiger and
PostgreSQL's own parser (libpg_query) itself, so it needs a Rust toolchain,
CMake, a C compiler and libclang, and the first build takes a few minutes.

The same crate embeds the server in a Rust test. `secantus_pg::PgServer::start()`
opens a temporary store and binds a free port; `.dsn()` and `.url()` hand the
address to `tokio-postgres` or any other client; dropping the value stops the
server and removes the store. It is safe to start and drop inside
`#[tokio::test]`. `PgServer::builder()` sets a persistent store, a port, extra
databases and the cache size when a test needs them.

```rust
let server = secantus_pg::PgServer::start()?;
let (client, conn) = tokio_postgres::connect(&server.dsn(), tokio_postgres::NoTls).await?;
tokio::spawn(conn);
client.batch_execute("CREATE TABLE greetings (id int PRIMARY KEY, text text)").await?;
```

Four crates went up: `secantus-pg`, and the planner, catalog and wire-protocol
crates it is built from. Those three are internal and carry no semver promise;
depend on `secantus-pg`. It pins the storage crates exactly, so
`secantus-mdb 0.5.3-beta.173` was released first, with new `secantusd-rs`
binaries for Linux, macOS and Windows. A crates.io build of either server uses
the same storage code and the same WiredTiger as the released binaries.

One change in this release is to WiredTiger itself. On macOS the PostgreSQL
server syncs every commit the way PostgreSQL does there, through a log file
opened `O_DSYNC`, and stock WiredTiger gives each of those commits a log write
of its own. Commits that arrive while a write is in flight now share the next
one. With eight clients inserting, each into its own table, that took the
server from 31.5k to 37.9k statements a second on the machine we measure on;
PostgreSQL 15 beside it did 41.5k. One to four clients are unchanged. A commit
is still acknowledged, and still becomes visible to other sessions, only after
its log record is written: a test kills the server under eight writers and
checks that every acknowledged row, and every row another session saw, is
there after restart. The patch applies to that one sync method. The MongoDB
servers, and the PostgreSQL server on Linux, sync differently and run
WiredTiger's own code.

The numbers that are not flattering belong here too. A read statement costs
about one and a half times PostgreSQL's on the same machine: `select 1` takes
about 38 microseconds against 26, a row by primary key about 44 against 28. A
durable `UPDATE` by primary key is level, 90 to 92 microseconds against 95 to
100. At eight clients durable writes scale about three times over one client
where PostgreSQL scales about four. We have not measured any of this on Linux.

This is a server for tests. It is single-node, it is a beta, and a role with
no password connects without one, so keep it on loopback.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.3) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.173)
