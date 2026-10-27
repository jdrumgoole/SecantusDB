### The Rust PostgreSQL server: lock waits end when the holder's COMMIT answers, and autocommit statements hold their table locks

Two lock bugs behind the `test_rp_isolation.py` failures that kept turning
CI red. In the first, a session waiting on a lock was still reported
blocked after the holder's COMMIT had answered. In the second, a DROP TABLE
and an autocommit UPDATE of the same table could run at once and stop the
server answering anyone.

#### Fixed

- `secantus-pg`: when a transaction ends, its lock waiters stop being
  reported as blocked by it before its COMMIT or ROLLBACK answers, as in
  PostgreSQL, where the releaser grants the waiters their locks. Before,
  each waiter cleared its own wait-for edge on its next poll (up to 10 ms
  later, more under load). In that gap `pg_isolation_test_session_is_blocked`
  and `pg_blocking_pids` still named the finished holder: 48 times in 50,
  where PostgreSQL 15.19 never does. The isolation tester asks that
  question right after each step. On the stale answer it sent the waiting
  session its next step, and psycopg refused it with "another command is
  already in progress". Replaying `unique-insert-wait`,
  `unique-column-insert-wait` and `on-conflict-where-lock` 3,840 times
  under load gave 38 to 58 such failures before the fix and none after.
  Fixes RP-270
  (`tests/backlog/test_rp_lock_release.py::test_a_waiter_is_not_blocked_once_the_holders_commit_answers`).
- `secantus-pg`: a statement outside a transaction block takes its table
  locks and holds them until it ends, as PostgreSQL's implicit transaction
  does. Before, it only waited out conflicting holds and took none. So an
  autocommit UPDATE waiting on a row and a DROP TABLE of the same table both
  went ahead once the row's holder committed. The drop held the storage's
  locks while it waited out the update's uncommitted write, and the update
  waited for those locks. Neither answered again, and every later BEGIN
  waited behind them. That wedge is what turned an isolation step's error
  into a worker killed at the 600 s per-test deadline. The other outcome of
  the race was a DROP that went first and an UPDATE that answered
  `UPDATE 0`. PostgreSQL 15.19 makes the DROP wait for the UPDATE, which
  answers `UPDATE 1`. Fixes RP-271
  (`tests/backlog/test_rp_lock_release.py::test_a_drop_waits_for_an_autocommit_update`).
