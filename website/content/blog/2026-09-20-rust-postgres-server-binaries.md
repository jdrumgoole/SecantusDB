Title: The Rust PostgreSQL server ships as a binary now
Date: 2026-09-20 12:00:00
Slug: rust-postgres-server-binaries
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantusd-pg has prebuilt archives for the first time, and the site now leads with the two Rust servers.

Until this week the only way to run SecantusDB's PostgreSQL server was to clone
the repository, check out a WiredTiger submodule and wait for `cargo build
--release`. There are prebuilt archives now — `secantusd-pg` for Linux x86_64
and macOS arm64, each with a `.sha256` alongside it. Download, extract, point it
at a directory and a port, and `psql` connects. There is no Windows build yet;
that one still builds from source.

It is the same storage engine underneath as the MongoDB server — the same
vendored WiredTiger, the same on-disk format in the same directory. The two
binaries are a hand-off rather than a pair: WiredTiger takes an exclusive file
lock, so the second server to open a store exits rather than sharing it. A table
written by `secantusd-pg` reads back through `secantusd-rs` as a collection with
the `PRIMARY KEY` as its `_id`, which is worth knowing before you assume the
arrow points both ways — it does not yet. The Rust SQL layer resolves names
through a catalog only `CREATE TABLE` writes, so a collection written by
`pymongo` is still `relation "..." does not exist` on the SQL side.

What the server answers is set by what a real client asks for rather than by a
feature list. All 5,729 of psycopg 3's own tests run against it unmodified;
5,545 pass, 183 skip as out of scope, and exactly one fails — a static-typing
check on psycopg's own class hierarchy that never reaches the wire. Whole type
suites a real application leans on pass end to end: `uuid`, `inet` and
`cidr`, enums, composites, ranges and multiranges, multidimensional arrays,
`numeric` exact well past 34 digits, `timestamptz` honouring the session zone —
each in text *and* binary format. So does the machinery around them: the
extended query protocol, prepared statements and portals, `COPY` in and out,
server-side cursors, `LISTEN` / `NOTIFY`, savepoints, and two-phase commit
durable across a restart.

One thing on that list changed late enough to be worth calling out, because we
had it documented wrongly on this very site until yesterday. A column-level
`UNIQUE` used to be accepted and then never enforced — a duplicate went in
silently where PostgreSQL answers `23505`. That is data corruption rather than a
missing feature, and it is fixed: every shape of it — column-level, table-level,
multi-column, named constraint — now raises PostgreSQL's own error, down to the
`DETAIL: Key (code)=(dup) already exists.` line.

Two gaps are still real and still deliberate. `CREATE INDEX` is **refused**
outright rather than accepted and ignored, so you find out in one run instead of
in a slow query six months later. And the server **does not verify passwords** —
a role's SCRAM verifier is stored and never checked, so a wrong password and no
password both connect. Keep it on loopback, and keep production data out of it.
The project's rule is a faithful "not supported" over a half-implemented feature
that diverges quietly; `UNIQUE` is what it looks like when something slips
through that rule, which is why it went in the open backlog rather than being
left for you to discover.

The site has been rebuilt around all this. The two Rust servers lead now — the
MongoDB one and the PostgreSQL one — with the pure-Python implementation
presented as what it has actually been for a while: the reference the Rust ports
are held to, the readable one, the place every operator and error message lands
first. Reach for Rust to run it; reach for Python to read it.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[Download the binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.1) ·
[The SQL interface](https://secantusdb.com/docs/sql.html)
