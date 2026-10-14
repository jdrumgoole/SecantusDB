Title: secantus-mdb 0.5.3-beta.194 checks what a user may run as mongod does
Date: 2026-10-14 18:00:00
Slug: secantus-mdb-0-5-3-beta-194
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.194 replaces its own permission check under --auth with mongod's: a privilege on a collection covers that collection, and every command needs the privileges mongod asks for.

Upgrade if you run `secantusd-rs` with `--auth`, and your tests check that
a user is refused something.

Before this release the server decided what an authenticated user could
run from a short list it kept itself. The list named a database and never
a collection. The last release's post gave two examples of what that got
wrong. This release replaces the list with `mongod`'s own rules.

- **A privilege on one collection covers that collection.** A role with
  `insert` on `app.orders` lets its user insert into `orders`, and nowhere
  else in `app`.
- **Every command needs a privilege.** `explain`, `bulkWrite`, `validate`,
  `collMod`, `fsync`, `killAllSessions` and a number of others needed
  none, so any logged-in user could run them. Each now needs what `mongod`
  asks for. A command the server does not know how to check is refused.
- **Arguments count.** An `update` with `upsert: true` needs `insert` as
  well. `findAndModify` needs `find`. A pipeline needs what each of its
  stages reads and writes: a `$lookup` needs `find` on the collection it
  joins, and an `$out` needs `insert` and `remove` on its target.
  `explain` needs what the command it explains needs. `bulkWrite` is
  checked one operation at a time.
- **Every built-in role grants what it grants on `mongod`.** That
  includes `hostManager` and `enableSharding`, which permitted nothing
  before.
- **Cursors belong to the user that opened them.** Another user can no
  longer read the next batch of a cursor or kill it.
- **Two listings are filtered rather than refused.** `listDatabases`
  lists only the databases the user has a privilege on.
  `listCollections` with `authorizedCollections: true` lists only the
  collections the user can reach.

We measured this by creating 66 users, each holding one role. Each user
sent the same commands to this server and to `mongod` 8.2.11, and we
compared the replies. Of 12,053 replies, the previous release differed
on 3,869. This one differs on 388. None of those 388 is about whether a
user may run a command:

- 324 are five commands this server does not have: `dbHash`,
  `logRotate`, `connPoolStats`, `setDefaultRWConcern` and
  `setFeatureCompatibilityVersion`.
- The rest are commands both servers allow, where the reply itself
  differs.

The server has a few commands of its own, such as `backupArchive` and
`pruneOplog`. `mongod` has no rule for these, so we chose one for each:

| command | privilege it needs |
| --- | --- |
| `backupArchive` | the cluster privilege `readBackupFile` |
| `restoreArchive` | `applyOps` |
| `pruneOplog` | `replSetResizeOplog` |
| `pruneTtl` | `compact` |

Without `--auth` nothing changes: every command is allowed, as before.

Users written by older releases still log in. We created users with the
0.5.3-beta.174 binary and opened the store with this one under `--auth`.
Each user logged in.

We ran the pymongo, Go and Node driver suites against this code. Each
gave the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.194` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.194)
