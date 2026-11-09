### A write's slow-query line counts mongod's interrupt checks

mongod counts every check it makes for `killOp` and `maxTimeMS` on an
operation's `Slow query` line, as `numInterruptChecks`, and a write makes
most of its checks as it finishes. A retryable write checks out its session.
A write that logged nothing, such as an update that matched no document or a
duplicate key, reads the system's latest optime as its client's last op,
under the global lock in IS. mongod reads it a second time when the client's
last op was already the latest. Then the write concern waits: for the oplog's
holes, for the journal flush under `j`, and for the commit point under
`majority`. Each wait checks before it waits and again when it wakes.

The Rust MongoDB server now does each of these where mongod does. It keeps
each connection's last op as mongod keeps a client's. It also counts the
check that mongod makes when a lock is taken again by the operation that
already holds it. On a member of a replica set, the line of an `insert`,
`update` or `delete` prints mongod 8.2's count: retryable or not, for
`w: 1`, `j` or `majority`, and for a write that writes and one that does
not.

#### Fixed

- `secantus-commands`:
  - a new `lastop` module holds the connection's last op (kept on
    `ConnectionAuth`), mongod's `LastOpFixer`, the entry point's read of the
    system's optime and the write-concern waits' checks;
  - a lock re-taken by its holder checks for interrupt, as
    `Locker::_lockBegin` does;
  - a retryable write's session check-out checks for interrupt;
  - the write planner (`explain::plan::write`) makes its planning checks:
    none for IDHACK, which mongod builds without its planner, so a `count`
    by `_id` prints its count too;
  - a write that logged nothing takes the global IS and transition locks of
    each optime read, which shows in its line's `locks` as on mongod;
  - `interrupt::finish` runs after the write-concern wait, whose checks can
    find the operation killed.
- `secantus-storage`: `take_last_logged_seq` reports the latest oplog entry
  this thread's committed writes logged (the PostgreSQL server links this
  crate and does not call it).
- Narrows RM-360 to multi-candidate plans, pipelines, `findAndModify`,
  inserts whose batch fails, unmodelled write concerns and DDL
  (tests/mongod_live/test_interrupt_checks.py::test_a_write_line_counts_mongods_interrupt_checks,
  tests/backlog/test_rm_355_364.py::test_every_line_counts_its_interrupt_checks).
  Narrows RM-357: a duplicate-key insert's `locks` are mongod's
  (tests/backlog/test_rm_355_364.py::test_more_writes_report_mongods_locks).
