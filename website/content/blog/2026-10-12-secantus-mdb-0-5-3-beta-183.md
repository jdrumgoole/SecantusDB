Title: secantus-mdb 0.5.3-beta.183 holds change streams to mongod's rules
Date: 2026-10-12 02:00:00
Slug: secantus-mdb-0-5-3-beta-183
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.183 checks what $changeStream is given, refuses a resume token it cannot find, names the transaction on a transaction's events, and reports an implicit create, after a comparison of the change-stream commands with mongod.

If your tests resume change streams on `secantusd-rs`, upgrade. Two things
in this release could leave a test passing on wrong data.

- **A resume token the server could not find resumed anyway.** `mongod`
  re-reads the token's event and refuses to resume (280,
  `NonResumableChangeStreamError`) when it is not the first event the
  stream sees: a token from another collection, or a pipeline that filters
  the token's event out. The server started after the token's position
  whatever the token named, so a test resuming with the wrong token read
  on as if nothing were wrong.
- **`startAtOperationTime` in the future delivered everything.** A stream
  asked to start at a time that had not come yet delivered the writes made
  before it. One asked to start far in the past, on a long oplog, delivered
  nothing.

We found these by sending the same scripts of change-stream commands to the
server and to `mongod` 8.2.11, and comparing every reply: what
`$changeStream` accepts, the shape of every event, `getMore`, the
invalidating operations, resuming, the three scopes, pre-images and
transactions. There are 115 scripts. The previous release answered 111 of
them differently. This one differs on 5.

**What the stream reports.**

- A write made in a transaction names it: its events lead with
  `txnNumber` and `lsid`, as `mongod`'s do.
- An insert that creates its collection produces a `create` event under
  `showExpandedEvents`, and so does an upsert.
- A stream on the target of a `renameCollection` with `dropTarget` sees
  the rename and is invalidated. It saw nothing. The replaced target no
  longer gets a `drop` event of its own.
- A cluster-wide stream no longer reports writes to `admin`, `config` and
  `local`.
- `collMod`'s `modify` event lists only what the command gave;
  `createIndexes` and `dropIndexes` events include the index's options in
  `mongod`'s order.
- A small event no longer carries `splitEvent: {fragment: 1, of: 1}` under
  `$changeStreamSplitLargeEvent`; `mongod` adds the field only to an event
  it split.

**What the commands refuse.** The `$changeStream` spec is checked as
`mongod` checks it: an unknown field, a wrong type, a bad `fullDocument` or
`fullDocumentBeforeChange` value, `resumeAfter` with `startAfter`, a token
whose `_data` is not a string or not hex, `allChangesForCluster` anywhere
but `admin`, and a stream on `admin`, `local`, `config` or a `system.*`
collection. A stage `mongod` does not allow after `$changeStream` is "not
permitted"; `$changeStreamSplitLargeEvent` must be last.

**The cursor.** The cursor document has `mongod`'s field order, with
`invalidated: true` on the batch that ends a stream. After a stream ends
its cursor is gone, and a `getMore` with cursor id 0 finds nothing. A 280
error leads with its `errorLabels`. Resume tokens are upper-case hex, as
`mongod`'s are; a token from an earlier release still resumes.

What this release does not do: decode a `mongod` resume token (the two
servers' tokens are their own); explain a change stream as the oplog scan
`mongod` would run; and `tasks/backlog.md` section 7.074 has the rest.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.183` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.183)
