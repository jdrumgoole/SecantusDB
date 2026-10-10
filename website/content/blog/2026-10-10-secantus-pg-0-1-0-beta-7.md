Title: secantus-pg 0.1.0-beta.7 and secantus-mdb 0.5.3-beta.175: one sync covers every commit already written
Date: 2026-10-10 09:00:00
Slug: secantus-pg-0-1-0-beta-7
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.7 and secantus-mdb 0.5.3-beta.175 share a WiredTiger change that lets one log sync cover every commit written before it, which makes durable writes on Linux 10 to 18 percent faster at eight and sixteen clients. The MongoDB server also gets read-only views that read through, capped collections that match mongod, and a slow-operation log.

Both Rust servers are released together because the change they share is in
the storage engine underneath them.

## One sync covers every commit already written

When several clients commit at once and each commit is synced to disk, the
commits queue for the log's `fdatasync`. A sync flushes everything written to
the log before it began, so the commits that were already written when it
started are durable when it returns. WiredTiger did not count them: it
recorded a sync as covering only the commit of the thread that made it, and
the next thread, whose bytes had just been flushed, synced again.

A sync is now credited with everything written before it started, as
PostgreSQL credits a WAL flush. Measured on a 16-vCPU DigitalOcean droplet
with the Rust PostgreSQL server, durable `UPDATE`s by primary key, two passes
before and two after:

| clients | commits per sync | statements a second |
| --- | --- | --- |
| 4 | 1.05 to 1.27 | 8,892 / 9,482 to 9,172 / 9,641 |
| 8 | 1.49 to 1.87 | 12,873 / 13,596 to 14,486 / 15,377 |
| 16 | 2.65 to 3.44 | 15,291 / 16,176 to 17,367 / 18,164 |

Durable writes at eight and sixteen clients are 10 to 18 percent faster. One
to four clients are unchanged, because there is rarely a second commit
waiting.

A commit is still acknowledged only after a sync that began after its bytes
were written. Killing the server cannot test that, because the operating
system's page cache survives the process and hands the data back. So we
hard-rebooted the machine under sixteen remote writers that logged every
acknowledged key. A build with the sync removed lost 5,538 and 5,359
acknowledged rows on two reboots, which shows the test can see a missing
sync. The released code lost none on six reboots, with 697,189 rows at the
end.

The change applies wherever each commit is synced with `fsync`: the
PostgreSQL server on Linux, and the MongoDB server when it is run with a sync
per commit. macOS syncs a different way and is unchanged. We measured it with
the PostgreSQL server only.

## Where the PostgreSQL server stands against PostgreSQL

Same droplet, PostgreSQL 16 at its defaults, statements a second with 1, 4
and 8 clients:

| | PostgreSQL 16 | secantusd-pg |
| --- | --- | --- |
| `INSERT`, one table per client | 5,473 / 14,647 / 21,639 | 3,426 / 10,812 / 15,624 |
| `UPDATE` by primary key | 4,902 / 13,393 / 21,409 | 2,832 / 9,193 / 14,350 |
| `SELECT` by primary key | 12,259 / 42,358 / 58,602 | 9,654 / 32,339 / 41,181 |

PostgreSQL is 1.3 to 1.7 times faster on the writes and 1.3 to 1.4 times
faster on the reads. Those are prepared statements. A statement sent as text
with a different literal each time is where the server is furthest behind:
380 microseconds against PostgreSQL's 128 for one row by primary key. That is
next.

The [server's page](https://secantusdb.com/rust-pg.html) had fallen behind
the server. It listed `CREATE INDEX`, `pg_constraint`, multi-column foreign
keys and password verification as missing, and all four work. It now lists
what the release binary answers, and what it does not: there is no TLS, a
role with no password connects without one, and there is no Windows build.

## The MongoDB server

`secantus-mdb` 0.5.3-beta.175 carries the same sync change and three pieces
of work measured against `mongod` 8.2.11.

**Views are read-only and read through.** A probe of 70 view operations found
52 different from `mongod`, several of them wrong answers with no error: an
`insert` aimed at a view was acknowledged and stored rows under the view's
name, `distinct` on a view returned nothing, and `$lookup` from a view
matched nothing. Two of the 70 differ now, both because the server has no
`system.views` collection.

**Capped collections match `mongod`.** A capped collection could grow past
its cap: `insert_many` of five documents into a `max: 3` collection left all
five, and an upsert was never held to the cap at all. `max: 0`, which means
no limit, was read as a limit of zero and emptied the collection on every
insert. A probe of 97 results went from 67 different to none.
`convertToCapped` is still not implemented.

**`$count` over no documents emits nothing**, as on `mongod`. It used to
emit `{n: 0}`.

**Slow operations are logged.** Any operation that runs for 100 ms or longer
writes a `Slow query` line with its namespace, command, outcome and duration.
The line does not yet say where the time went, as `mongod`'s does.

Aggregations are also faster. Over 10,000 five-field documents on a Mac, a
multi-stage pipeline went from 13.1 ms to 10.1 ms (`mongod`: 4.6 ms), and
`$match` then `$count` from 6.2 ms to 3.0 ms (`mongod`: 1.4 ms).

These are servers for tests. They are single-node, and they are betas.

`cargo install secantus-pg --version 0.1.0-beta.7` and
`cargo install secantus-mdb --version 0.5.3-beta.175` build them from
crates.io. Binaries are on the releases.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.7) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.175)
