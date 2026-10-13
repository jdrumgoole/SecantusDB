Title: secantus-mdb 0.5.3-beta.192 answers the handshake and diagnostic commands as mongod does
Date: 2026-10-13 22:00:00
Slug: secantus-mdb-0-5-3-beta-192
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.192 fixes dataSize over a key range, the sizes in $collStats and the counters in serverStatus, and checks the fields every command takes, as mongod does.

Upgrade if you run `secantusd-rs` and your tests read what the server says
about itself, or rely on it refusing a malformed command.

Three answers were wrong:

- **`dataSize` with `min` and `max` sized the whole collection.** It sizes
  the documents of that key range now.
- **The `storageStats` of a `$collStats` stage said size 0** for every
  collection. They are the collection's sizes now, the same numbers the
  `collStats` command gives.
- **`serverStatus` reported zeros.** The process id, the uptime, the
  operation counters and the network bytes were each a constant 0. They
  are counted now. A test that reads `opcounters` before and after an
  operation sees them move.

Several things `mongod` refuses are refused here now:

- **A field of the wrong type.** `$readPreference`, `$clusterTime`,
  `apiVersion`, `apiStrict`, and `afterClusterTime` and `atClusterTime` in
  a read concern were each accepted with any value and then ignored.
- **A snapshot read at a time long past.** A read at an `atClusterTime`
  more than five minutes old was answered with the current documents. It
  is `SnapshotTooOld` now.
- **A field a command does not have.** `{ping: 1, bogus: 1}` ran.
- **A command outside API Version 1 under `apiStrict: true`.** The server
  kept its own list, and ten of its names were wrong, `collStats` and
  `createUser` among them. The list is `mongod`'s own now.
- **Driver metadata on a second `hello`**, and metadata without a driver
  name or an operating system type.

The handshake reply changed. `hello` answers `isWritablePrimary` and no
longer `ismaster`; `isMaster` answers `ismaster` and no longer
`isWritablePrimary`. That is what `mongod` sends. A current driver reads
the one that matches the command it sent.

Six commands are new: `listCommands`, `logout`, `getDefaultRWConcern`,
`replSetGetConfig`, `shardingState` and `fsyncUnlock`. `whatsmyuri` gives
the client's real address; it said port 0 to everyone.

Three things the server took before and now refuses, because `mongod`
does: a string of digits as the `op` of `killOp`, `wtimeoutMS` inside a
write concern, and a `filter` document on the `profile` command, which was
accepted and then not applied.

We found these by sending 1,141 commands about the server and the
connection to this server and to `mongod` 8.2.11 and comparing each reply.
The previous release differed on 492. This one differs on 160.

What this release does not do. Most of the 160 are things the server does
not keep: the sections of `serverStatus` beyond connections, counters and
network; the several hundred server parameters `mongod` has, of which
this server reports thirteen; and replica-set and sharding commands. And
one is not about a reply: `validate` answers `valid: true` without
checking the collection. It did so before this release too. Do not use it
to test that a store is sound.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.192` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.192)
