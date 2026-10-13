Title: Both Rust servers now trim their log while they run
Date: 2026-10-13 22:00:00
Slug: rust-servers-trim-their-log
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.191 and secantus-pg 0.1.0-beta.15 checkpoint every 60 seconds. Before them a running server kept every log file it had written and replayed all of them after a crash. After 14 minutes of updates, secantusd-rs took 81 s to restart after a kill and now takes 4 s; secantusd-pg took 37 s and now takes 1.4 s. Throughput is unchanged.

Upgrade if you leave either Rust server running under writes for more than
a few minutes: a long test run, a shared development server, a container
that stays up. A server that starts, runs one test suite and exits was
never affected much.

## What was wrong

Neither `secantusd-rs` nor `secantusd-pg` took a checkpoint between opening
its store and closing it. WiredTiger removes a log file only after a
checkpoint has made it unnecessary. So a server under writes kept every log
file it had ever written, and after a crash it replayed all of them before
it would accept a connection.

No data was lost by this: the log held the commits and was replayed. The
cost was disk space that grew without limit and a restart
that took longer the longer the server had been up.

We found it by watching a store's size during a benchmark that was looking
for something else.

## What changed

Both servers checkpoint every 60 seconds. `SECANTUS_CHECKPOINT_SECONDS`
sets the interval on either server, and `secantusd-rs` also takes
`--checkpoint-seconds`.

## Measured

Each server was built twice on one droplet (16 vCPUs, Xeon Platinum 8280),
with and without the change, and given eight clients updating one document
at a time from two other droplets. Then the server was killed with SIGKILL
and started again.

`secantusd-rs`, sixty 10 s trials, pymongo clients:

| | before | after |
| --- | --- | --- |
| updates a second, median | 15,690 | 15,724 |
| slowest trial | 14,865 | 15,046 |
| store at the end | 4.1 GB, 2 log files | 2.1 GB, 1 log file |
| time to listen after the kill | 81.2 s | 3.9 s |

`secantusd-pg`, forty 10 s trials, psycopg clients, every commit synced to
disk:

| | before | after |
| --- | --- | --- |
| statements a second, median | 13,815 | 13,864 |
| slowest trial | 13,248 | 11,660 |
| store at the end | 2.1 GB, 132 log files | 69 MB, 4 log files |
| time to listen after the kill | 36.8 s | 1.4 s |

The checkpoint costs no throughput we could measure on either server. One
of the forty `secantusd-pg` trials with the change came in 15% under the
median; the next slowest was 13,310, and none of the sixty `secantusd-rs`
trials did the same.

The two servers size their log files differently, which is why the tables
look different. `secantusd-rs` writes 2 GB files and allocates each in full
when it is created, so its store is never smaller than 2 GB once it has
taken a write. Before the change it added another 2 GB about every six
minutes of this load and kept them all.

After each restart, on both servers and both builds, every document was
there.

## What it does not change

- A store opened for a test with `durable=false` does not checkpoint while
  open, as before.
- The Python servers have the same gap and are not changed by this
  release. We have not measured them.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.191` and
`cargo install secantus-pg --version 0.1.0-beta.15` build them from
crates.io. Binaries are on the releases: Linux x86_64, macOS arm64 and
Windows x86_64 for the MongoDB server, Linux x86_64 and macOS arm64 for the
PostgreSQL server.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.191) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.15)
