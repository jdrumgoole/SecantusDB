Title: secantus-pg 0.1.0-beta.6 makes durable writes on Linux about half again as fast
Date: 2026-10-09 21:00:00
Slug: secantus-pg-0-1-0-beta-6
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.6 zero-fills its log on Linux, reuses the plan of a prepared write into a bigint column, and allocates with mimalloc. Durable writes on Linux go from about 2 to 2.4 times slower than PostgreSQL to 1.3 to 1.7 times through eight clients.

On Linux, `secantusd-pg` 0.1.0-beta.6 runs durable `INSERT`s and `UPDATE`s a third
to two thirds faster than 0.1.0-beta.4 did. Three changes add up to that, and
one of them is most of it.

**The log file is zero-filled.** Every durable commit ends in an `fdatasync`
of the log. With several clients writing, both this server and PostgreSQL are
limited by that one stream of syncs, and ours were slower: about 150
microseconds each against PostgreSQL's 100 on the machine we measured.
WiredTiger reserves its log file on disk without writing it, so the first
write into each block makes the filesystem record that the block now holds
data, and the sync has that to commit as well. PostgreSQL avoids this by
writing zeros through a new WAL segment, and the server now does the same.
The sync dropped to about 110 microseconds.

The log files are 16 MB each where they were 128 MB. Zero-filling 128 MB added
150 milliseconds to every start, and 16 MB adds 25, with the same throughput.
A new store also takes 17 MB on disk where it took 129. A transaction larger
than one file spans files; we committed a 60 MB one, killed the server and
found all of it after restart. A store written by an earlier release opens
unchanged, and an earlier release opens a store written by this one. This
applies on Linux when commits sync, which is the default. macOS syncs a
different way and is unchanged.

**A prepared write into a `bigint` column reuses its plan.** A driver sends a
small integer as `smallint` or `integer`, and bound for a `bigint` column the
server widens it. A prepared `INSERT` or `UPDATE ... SET v = $1` that did so
was planned again on every execution. It is now planned once. Counted in CPU
instructions per statement, such an `INSERT` went from 381 thousand to 314
thousand.

**The binary allocates with mimalloc**, as the MongoDB server's does. That
removes 13 to 17 percent of the instructions in every statement we timed. It
raised throughput 5 to 11 percent with one or two clients and not measurably
with four or more, where the sync is the limit. A program that embeds
`secantus_pg::PgServer` keeps its own allocator.

The numbers, in statements a second with 1, 2, 4 and 8 clients, each client
writing its own table, on a 16-vCPU DigitalOcean droplet with PostgreSQL 16 at
its defaults. The beta.4 figures are from a different droplet of the same size
earlier the same day:

| | 0.1.0-beta.4 | 0.1.0-beta.6 | PostgreSQL 16 |
| --- | --- | --- | --- |
| `INSERT` | 2,395 / 4,295 / 6,396 / 8,299 | 3,355 / 6,193 / 9,726 / 13,767 | 4,795 / 8,211 / 13,345 / 20,862 |
| `UPDATE` by primary key | 2,213 / 3,972 / 6,333 / 8,381 | 2,963 / 5,297 / 8,769 / 12,543 | 4,623 / 7,532 / 13,669 / 20,780 |

The server is still slower than PostgreSQL: 1.3 to 1.5 times on these
`INSERT`s and 1.4 to 1.7 times on the `UPDATE`s. At sixteen clients it is
about twice as slow, 14,504 `UPDATE`s a second against 31,335. There
PostgreSQL fits more commits into each sync than we do, 5.7 against 3.1, and
that is the next thing to work on. A single durable `UPDATE` costs 334
microseconds against PostgreSQL's 188, and a prepared read of one row 137
against 60.

This is a server for tests. It is single-node, it is a beta, and a role with
no password connects without one, so keep it on loopback.

`cargo install secantus-pg --version 0.1.0-beta.6` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.6)
