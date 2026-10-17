### Rust MongoDB server: a real lock manager — `serverStatus.locks`, and DDL that waits on open transactions

The Rust MongoDB server had no lock manager at all: `serverStatus.locks`
stayed absent ("there is no lock manager to count"), and a `drop` (or
`createIndexes`, `renameCollection`, `collMod`) removed or rewrote a
collection while a transaction that had read or written it was still open —
mongod 8.2.11 makes the DDL wait, answers 50 `MaxTimeMSExpired` at its
`maxTimeMS`, and keeps the collection.

There is a real lock manager now (`secantus-commands/src/lock_manager.rs`),
modeled on mongod's (mongo/db/concurrency): resources are `Global`,
`Database`, `Collection` and the special ones mongod reports, in the four
modes mongod's letters name (`r` intent-shared, `w` intent-exclusive, `R`
shared, `W` exclusive), taken hierarchically — every statement passes the
global lock, a collection-addressing statement takes its database's intent
and the collection's (reads intent-shared, writes intent-exclusive, DDL
exclusively; `createIndexes` takes the collection exclusively, matching the
recorded decision and mongod's non-concurrent build, so a writing
transaction's lock blocks it). A statement inside a transaction takes its
chain under the transaction's owner and keeps the collection intents to
commit or abort, so a concurrent DDL command then waits on exactly the
collection the transaction touched — read or written — and its deadline
answers 50. Waits poll the request's `maxTimeMS` budget, so a timed wait
ends with `MaxTimeMSExpired` instead of outliving it.

`serverStatus.locks` is generated from the manager's counters — per
resource, per-mode `acquireCount`, and `acquireWaitCount` /
`timeAcquiringMicros` for the modes that waited — never a table of
plausible counts (RM-034's recorded decision). The manager is per-server
and built into the server's shared state, like the transaction registry.

#### Fixed

- `drop` under an open transaction that read or wrote the collection waits
  and answers 50 at its `maxTimeMS`, keeping the collection, as mongod
  8.2.11 does; so do `createIndexes`, `renameCollection` and `collMod`
  (RM-084; the gap tests
  `tests/backlog/test_rm_057_084.py::test_drop_waits_for_a_transaction_that_read`
  and `::test_drop_under_a_transaction_that_wrote_honours_max_time` are
  fixed-marked now, and `crates/secantusdb/tests/embedded.rs::ddl_waits_for_an_open_transaction_and_answers_50_at_max_time`
  pins the four commands over the wire).
- `serverStatus.locks` is reported from the lock manager's acquisition
  counters (RM-034; `tests/backlog/test_rm_029_056.py::test_server_status_sections`
  fixed-marked).

#### Added

- `secantus-commands/src/lock_manager.rs`: mongod's resource/mode lock
  model with acquisition counting and deadline-bounded waits; statement
  chains are taken at dispatch (`run_handler`) and released at the
  statement's end, transactions hold their collection locks to commit/abort
  (the registry releases them on every terminal path, including the idle
  reaper), and unit tests pin the conflict table, wait counting, and the
  deadline's 50.
