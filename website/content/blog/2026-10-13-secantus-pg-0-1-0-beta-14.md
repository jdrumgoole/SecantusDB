Title: secantus-pg 0.1.0-beta.14: a faster build, and a benchmark that was unfair to PostgreSQL
Date: 2026-10-13 14:00:00
Slug: secantus-pg-0-1-0-beta-14
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.14 answers the same as 0.1.0-beta.13 and is built differently: with link-time optimisation, and for the released binaries a profile. Eight clients selecting by primary key get 34,288 statements a second from 0.1.0-beta.13 and 41,038 from this release, on one droplet. The same work found that our published comparison had PostgreSQL encrypting every statement while our server did not. The table is measured again with both in the clear, and PostgreSQL's lead on reads is 1.13x.

This release changes no answer the server gives. It changes how the binary
is compiled, and it corrects a measurement we had published.

## Where one connection's time went

On a 16-vCPU droplet, one client sending one statement at a time got less
from the Rust PostgreSQL server than the eight-client rows suggested it
should. The guess was that a statement crossed more threads here than in
PostgreSQL. That guess was wrong. One server thread sleeps and wakes once a
statement, as a PostgreSQL backend does, and it makes three system calls
where PostgreSQL makes five.

The time was processor time on that one thread: 61 microseconds a statement
against PostgreSQL's 22. For a select by primary key the server ran about
97,000 instructions where PostgreSQL ran 35,000, and ran them at 0.54
instructions a cycle where PostgreSQL managed 0.91. The profile was flat,
with nothing above 3%, so there was no one function to fix.

A flat profile with a low rate of instructions a cycle is what a compiler
can help with, and the released binary had been compiled at Cargo's
defaults: each crate optimised alone, and no knowledge of which paths run.

## What the build does now

One pinned client on that droplet, statements a second:

| Build | select | update | insert |
| --- | --- | --- | --- |
| 0.1.0-beta.13 as released | 12,052 | 3,075 | 3,992 |
| with link-time optimisation | 14,402 | 3,577 | 4,441 |
| and a profile | 17,855 | 4,472 | 5,418 |
| PostgreSQL 16 | 22,159 | 6,644 | 7,140 |

Link-time optimisation is now the crate's release profile, so `cargo install
secantus-pg` has it. The profile is collected when a release is built: an
instrumented binary runs a workload of the statements a test suite sends,
and the binary that ships is compiled with what that run recorded. `cargo
install` cannot have the profile, so the prebuilt binary is the faster of
the two.

The archive is smaller as well: 11.8 MB for Linux, from 15.7.

The cost is build time. `cargo install` took three and a half minutes on an
M-series Mac. A build of the server as a test dependency is a debug build
and is unchanged.

## The benchmark was unfair to PostgreSQL

The table on the [Rust PostgreSQL page](https://secantusdb.com/rust-pg.html)
compares a released binary with PostgreSQL 16 on one droplet. Ubuntu's
PostgreSQL offers TLS, and psycopg takes TLS when a server offers it. Our
server offers none unless it is started with a certificate. So PostgreSQL
encrypted every statement in that table and our server encrypted none.

One client selecting by primary key got 16,988 statements a second from
PostgreSQL with TLS and 22,159 without. The published ratios were too kind
to us, most of all on reads. The benchmark now turns TLS off for both
servers, records what PostgreSQL reports about its own connection, and
stops if that connection was encrypted.

## The table, measured again

Both releases against PostgreSQL 16.15 on one droplet (16 vCPUs, Xeon
Platinum 8280), eight clients on two other droplets, every commit synced to
disk, no TLS. Statements a second:

| Workload | PostgreSQL 16 | 0.1.0-beta.13 | 0.1.0-beta.14 | PostgreSQL is faster by |
| --- | --- | --- | --- | --- |
| insert, one table each | 19,647 | 14,140 | 15,514 | 1.27x |
| insert, shared table | 19,358 | 13,301 | 14,774 | 1.31x |
| update by primary key | 19,364 | 12,542 | 14,366 | 1.35x |
| select by primary key | 46,326 | 34,288 | 41,038 | 1.13x |

The 0.1.0-beta.14 and PostgreSQL columns are medians of seven 10 s trials,
and the ratio is between those two. The 0.1.0-beta.13 column is the median
of five, from a second run on the same droplets an hour later.
Its update row is from a repeat of seven trials. In the first run three of
that row's five trials fell to between 8,000 and 9,000; the repeat did not
show it, and we have not found the cause.

PostgreSQL is faster on every row. The table we published for
0.1.0-beta.13 said 1.17x on reads; with PostgreSQL in the clear, the same
release is 1.34x behind, and this one 1.13x.

## The disk is half the result

A droplet of a given size is not one machine. The first droplet created for
this measurement had a different processor and a disk that completed 1,700
`fdatasync` calls a second, where the one above completed 12,388. On it one
client's inserts ran at 613 a second on PostgreSQL and 568 on our server. A
commit that waits for the disk waits the same on both, so a slow disk makes
any two servers look alike. Each run now records the processor and the
disk's rate beside its results, and we compare two runs only when those
agree.

## The checks that ran

- psycopg 3.3.4's own test suite, unmodified, against the released macOS
  binary: 5,544 pass and none fail of 5,731, the same counts as
  0.1.0-beta.13.
- The release workflow's smoke test, through a restart, on both binaries
  before they were attached.
- `cargo install secantus-pg --version 0.1.0-beta.14` from crates.io, and a
  query against what it built.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.14` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.14)
