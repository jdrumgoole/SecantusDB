### A kill interrupts the operation it names, mid-expression, and answers as mongod does

The Rust MongoDB server now interrupts a running operation the way mongod
8.2 does. `killSessions`, `killAllSessions` and `killAllSessionsByPattern`
interrupt the operations running on the sessions they kill, not only their
transactions and cursors; `killOp` and a spent `maxTimeMS` reach an
operation inside one long expression, at each element a `$map`, `$filter`
or `$reduce` walks, where before they were read only between the documents
a scan reads, so a slow filter on one document ran to the end. An
interrupted write answers with mongod's reply and leaves what mongod
leaves. Measured against mongod 8.2.11 on a replica set and a standalone
(2026-10-27); `tests/mongod_live/test_interrupts.py` compares every case.

#### Fixed

- `killSessions` on a session whose retryable update is running interrupts
  it: 11601 "Plan executor error during update :: caused by :: operation
  was interrupted", nothing applied, and a retry runs the statement. Fixes
  RM-313 (`tests/backlog/test_rm_found_2026_10_25.py::test_kill_sessions_interrupts_the_command_holding_the_session`).
- An interrupted `update` or `delete` fails the whole command, ordered or
  not, under its executor wrapper; the statements before the interrupted
  one stay applied, and for a retryable write recorded, so a retry answers
  for them (`retriedStmtIds`) and runs the rest. An `insert`'s error is
  bare, and `findAndModify`'s is bare on a replica set and wrapped on a
  standalone. The same shapes hold for `maxTimeMS`, which `update`,
  `delete` and `findAndModify` answered bare and unwrapped.
- An insert, update or delete outside a transaction whose write concern
  waits (the default, `majority`, `j`, `fsync`) carries mongod's
  `writeConcernError` when killed, on an `ok: 1` reply too when the kill
  landed after the write; `w: 1` carries none.
- A killed statement of a transaction aborts the transaction (251
  `NoSuchTransaction` with `TransientTransactionError` afterwards), for
  `killOp` as for `killSessions`, and `killSessions` no longer waits for
  the statement to finish before it answers.
- A retryable insert checks for interrupt before its batch commits, and a
  kill rolls the batch back, as mongod's does; a plain insert, whose
  validator no interrupt reaches on mongod, finishes. A collection
  validator is evaluated where no interrupt reaches it, so an interrupt can
  no longer read as a document that passes.
- A running `getMore` killed by `killSessions` or `killOp` answers 11601
  under its executor wrapper, and its cursor is gone.
- An `updateMany` whose filter fails part-way, by an interrupt or an error
  on one document, leaves the documents before it updated, as mongod's
  does, and a `deleteMany` the batches of ten it had deleted; both wrote
  nothing. Each document's filter is now evaluated once, not once by the
  scan and again by its write. Fixes RM-322
  (`tests/backlog/test_rm_found_2026_10_27.py::test_an_update_many_failing_part_way_keeps_the_documents_before`,
  `::test_a_delete_many_failing_part_way_keeps_the_batches_before`).

- `$currentOp` lists a running `count`, `distinct` or `findAndModify`
  under its collection's namespace, as mongod does, not under `<db>.$cmd`;
  a `$match` on the namespace found nothing.
- A `drop` or `createIndexes` waiting for a transaction's lock ends when
  killed (bare 11601; `createIndexes` with a `writeConcernError`); it waited
  on.

#### Known gaps

- A multi-document write interrupted while it reads its candidates, or a
  `deleteMany` on a replica set interrupted anywhere, still writes nothing
  where mongod keeps what it wrote (RM-323).
- `$currentOp` lists a running `bulkWrite` as the whole command, where
  mongod lists the op it is running (RM-324).
