### Rust MongoDB server: sessions, transactions and retryable writes follow mongod's rules

We compared the Rust MongoDB server with `mongod` 8.2.11 on 251 scripts of
session, transaction and retryable-write commands. 189 differed. 11 do now.

Two of the differences let a write through that should have been refused.
A command whose session fields were malformed, such as `autocommit: true` or
a `txnNumber` with no session, was run as an ordinary write. And a collection
created inside a transaction with a validator took any document for the rest
of that transaction. A third gave a wrong reply: a retried batch that had
failed part-way was run again from the start, so the retry reported a
duplicate key for the document its own first attempt had inserted.

#### Fixed

- The session fields (`lsid`, `txnNumber`, `autocommit`, `startTransaction`)
  are checked on every command, with `mongod`'s codes and messages. A
  `txnNumber` must be an int64.
- A transaction's state errors are `mongod`'s: 225 for an older number on any
  request, 117 for `startTransaction` at the active number, 251 naming the
  session and its active number or saying the transaction was aborted, with
  `TransientTransactionError` first in the reply. Transaction number 0 is
  valid.
- A command a transaction does not allow (`count`, `listCollections`, `ping`
  and the rest) is refused without aborting the transaction, and as a first
  statement starts none. A statement that fails while running still aborts.
- `commitTransaction` and `abortTransaction` need the `admin` database and a
  transaction (13, 125).
- In a transaction: `$unionWith`, a tailable `find`, `create` of a view or a
  time-series collection, `createIndexes` on an existing or non-empty
  collection, an insert into a time-series collection, the `config` and
  `local` databases and system collections are refused as `mongod` refuses
  them. An unordered batch stops at its first error.
- A collection created in a transaction is held to its validator by the
  transaction's later statements.
- A retryable write is remembered per statement and only if it wrote. A retry
  answers `retriedStmtIds` (`retriedStmtId` for `findAndModify`) and runs
  only the statements that had not been applied. `stmtId` and `stmtIds` are
  honoured. An older number is 225, a `multi` update or a `limit: 0` delete
  is refused, and a `txnNumber` on a command that is not a write is 50768.
- A cursor can be read on only by the session, and the transaction, that
  opened it (50736 to 50741). `killSessions` kills the session's cursors.
- `endSessions` no longer aborts the session's open transaction; `mongod`
  leaves it to the client or to the transaction lifetime.
- A transaction idle for its lifetime (60 seconds) is aborted about a second
  later, whatever else the server is doing. It used to be aborted only when
  another transaction command arrived, so a writer waiting on a document an
  abandoned transaction held could wait for ever.
- A `commitTransaction` whose commit failed in the storage engine answered
  `ok: 1`. It now reports the error and the transaction is aborted.
- A malformed `readConcern` gets `mongod`'s error, and a command that is not
  a read refuses a level above `local`.
- A write sent with `w: 0` reports `n: 0` and no write errors.
- An insert, update or delete reply lists `writeErrors` before `upserted`.
- `$geoNear` with no geo index answers 27 `IndexNotFound`.
