### Concurrency stress suites hammer the servers — and the races they caught are fixed

Two new concurrency harnesses hammer the servers with barrier-synchronized
thread storms — one drives the Mongo-wire servers (the Python server and the
embedded Rust server, every test parametrized over both) through real pymongo
clients, the other drives the PostgreSQL-wire server through psycopg —
same-key insert races, transactional increment hammers, bank-transfer
invariants under concurrent readers, findAndModify ticket dispensers, unique-
index races, DDL churn against live writers, and connection churn under load.
Every test asserts a hard integrity invariant (exact counts, exactly one race
winner, a conserved total) plus error hygiene: the only errors a loser may see
are the typed, retriable signals a real server would send.

The suites caught four real concurrency bugs, now fixed. A SQL write-write
conflict escaped as a generic `XX000 internal error`; it now surfaces as
SQLSTATE `40001 serialization_failure`, the retriable signal drivers key
their retry loops on, and the losing connection stays fully usable. SQL DML
statements are read-modify-write sequences spanning several storage calls, so
concurrent inserts could double-satisfy a `UNIQUE` constraint (134 rows landed
for 30 distinct values in the reproducer) and concurrent `SET n = n + 1`
updates lost increments (83 of 400 survived); DML statements — and bare
`nextval()` draws — now serialize per shared storage, closing both. And
`findAndModify {new: true}` on the Python server re-found the document after
updating it, so two concurrent callers could be handed the same post-image
(8 duplicate tickets in 400); the write now captures its own post-image while
it holds the storage lock. The Rust server passes the same storms except the
findAndModify ticket test, which stays xfail pointing at the atomic
find-and-modify primitive it still needs (tasks/backlog.md).

#### Added

- `tests/test_pgserver_concurrency.py` — 11 psycopg-driven stress tests:
  autocommit insert storms, same-PK and UNIQUE-constraint races (exactly one
  winner, losers see `23505`), transactional and autocommit increment hammers,
  a deterministic two-transaction `40001` conflict, bank transfers conserving
  the total under concurrent readers, concurrent `nextval()`, DDL churn
  alongside DML, connection churn under write load, extended-protocol prepared
  statements across threads, and a bounded txn-vs-autocommit stall check.
- `tests/test_mongo_server_concurrency.py` — one pymongo-driven harness
  parametrized over BOTH Mongo-wire servers (the pure-Python
  `SecantusDBServer` and the embedded Rust server): insert storms, `$inc`
  hammers, findAndModify ticket dispensers, upsert races, unique-index races,
  readers paginating (`getMore`) through churn, index builds under write
  load, multi-collection writers, delete/insert churn, client connection
  churn, and a change stream observing every insert from four concurrent
  writers. Every test runs against both servers.
- `Storage.update_matching(..., return_post_images=True)` — returns the
  post-image of each write, captured while the statement holds the storage
  lock, so command handlers never re-read what they just wrote.

#### Fixed

- SQL: a storage-level write-write conflict (`WriteConflictError` /
  `WT_ROLLBACK`) now maps to SQLSTATE `40001 serialization_failure` on both
  the simple and extended protocol paths, instead of escaping as `XX000
  internal error`; the losing connection survives, `ROLLBACK` works, and
  retry converges.
- SQL: DML statements serialize per shared storage, so concurrent inserts can
  no longer double-satisfy a `UNIQUE` constraint and concurrent computed
  updates (`SET n = n + 1`) no longer lose increments. Bare
  `SELECT nextval('seq')` draws are serialized the same way and never repeat
  a value.
- `findAndModify {new: true}` (Python server) returns the post-image of its
  own write instead of a racy re-read, so concurrent callers can no longer be
  handed the same ticket; the upsert path returns the upserted document from
  the write itself.
