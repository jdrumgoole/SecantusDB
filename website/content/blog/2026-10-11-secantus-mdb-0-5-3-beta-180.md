Title: secantus-mdb 0.5.3-beta.180 holds transactions and retryable writes to mongod's rules
Date: 2026-10-11 19:00:00
Slug: secantus-mdb-0-5-3-beta-180
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.180 checks the session fields on every command, remembers a retryable write statement by statement, reports a commit that failed, and aborts an abandoned transaction when its lifetime runs out.

If your tests use transactions or retryable writes on `secantusd-rs`, upgrade.
Four things in this release could leave a test passing on wrong data.

- **A failed commit answered `ok: 1`.** When the storage engine refused a
  `commitTransaction`, the error was dropped and the client was told the
  transaction had committed. It is now reported, and the transaction is
  aborted.
- **A malformed transaction ran as an ordinary write.** A command with
  `autocommit: true`, or a `txnNumber` and no session, inserted its document.
  `mongod` refuses it. So does the server now.
- **A validator created inside a transaction was not applied in it.**

```python
with client.start_session() as s, s.start_transaction():
    db.create_collection("orders", validator={"total": {"$gte": 0}}, session=s)
    db.orders.insert_one({"total": -5}, session=s)   # was accepted
```

- **An abandoned transaction could block a writer for ever.** A transaction
  nobody ended was only cleared when another transaction command arrived. A
  write waiting on a document it held waited until then. The server now
  aborts a transaction that has been idle for its 60-second lifetime, as
  `mongod` does.

We found these by sending the same scripts of session, transaction and
retryable-write commands to the server and to `mongod` 8.2.11, and comparing
every reply. There are 251 scripts. The previous release answered 189 of
them differently. This one differs on 11.

**Transactions.**

- The session fields (`lsid`, `txnNumber`, `autocommit`,
  `startTransaction`) are checked on every command, with `mongod`'s errors.
- A command a transaction does not allow, such as `count` or
  `listCollections`, is refused and the transaction stays open. The server
  used to abort it. A statement that fails while running, such as a
  duplicate key, still aborts it.
- The errors for a transaction that is too old, already committed or
  already aborted are `mongod`'s, code and text, with
  `TransientTransactionError` where `mongod` puts it.
- `commitTransaction` and `abortTransaction` need the `admin` database.
- Inside a transaction the server now refuses what `mongod` refuses:
  `$unionWith`, a tailable cursor, creating a view or a time-series
  collection, building an index on a collection that already exists, and
  writes to the `config` and `local` databases.

**Retryable writes.** A driver retries a write by sending it again with the
same `txnNumber`. The server remembered the whole command and its reply.
`mongod` remembers each statement that wrote, and so does the server now.

- A retried batch that failed part-way answers for the statements that went
  in (`retriedStmtIds`) and runs the rest. It used to run the batch again
  from the start and report a duplicate key for its own first document.
- A statement that wrote nothing, such as an update that matched no
  document, runs again on a retry.
- `stmtId` and `stmtIds` are honoured.
- A `txnNumber` older than the session's latest is refused, and so is a
  `multi` update or a `limit: 0` delete sent as a retryable write.

**Cursors and sessions.** A cursor can be read on only by the session, and
the transaction, that opened it. `killSessions` kills the session's cursors.
`endSessions` no longer aborts the session's open transaction, because
`mongod` does not: the transaction ends when the client ends it or when its
lifetime runs out.

Three smaller replies changed. A write sent with `w: 0` reports `n: 0`. A
malformed `readConcern` gets `mongod`'s error, and a command that is not a
read refuses a level above `local`. An insert, update or delete reply lists
`writeErrors` before `upserted`.

What this release does not do:

- `drop` and `createIndexes` do not wait for an open transaction on the
  same collection. On `mongod` they wait.
- A change stream event from a transaction has no `txnNumber` and `lsid`.
- When two transactions both create the same collection, the second fails
  at its first insert. On `mongod` it fails at commit.
- A retried `bulkWrite` command runs again.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.180` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.180)
