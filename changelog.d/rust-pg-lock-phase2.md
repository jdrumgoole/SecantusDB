### Rust PostgreSQL server: one wait-for graph for every lock kind, and lock waits you can see

Two related backlog items close together. RP-048: a deadlock cycle that ran
through two lock kinds — a session holding an advisory lock and waiting for
a row, blocked by one holding the row and waiting on the advisory lock —
was invisible to both waits' own cycle checks, and both sessions waited out
`lock_timeout` (or forever) where PostgreSQL raises `40P01` at
`deadlock_timeout`. The row and table lock waits now register in the same
process-wide wait-for graph the advisory waits joined in the advisory
deadlock work: `row_waits::Waiting` is a thin wrapper over the graph's
guard, and `table_locks`' wait publishes its real holder edges each poll.
One depth-first search serves every lock kind; only real holder edges are
registered, and the once-a-second, first-waiter-loses timing the deadlock
slice tests pin is unchanged. A cross-kind cycle is `40P01` in the first
waiter, measured end to end (the gap test previously saw both sides answer
`55P03` after their `lock_timeout`).

RP-047: what an operator reads while a session is blocked. The implicit
statement table locks take PostgreSQL's modes now — `FOR UPDATE` / `FOR
SHARE` take `RowShareLock`, `UPDATE` and `DELETE` take `RowExclusiveLock`
alone, `TRUNCATE` holds `ShareLock` beside its `AccessExclusiveLock` — and
`pg_locks` shows each running transaction's `virtualxid` and (assigned at
its first write) `transactionid` locks, plus a row waiter's ungranted
`ShareLock` on the holder's transaction id, the `XactLockTableWait` shape
that is how row waits reach PostgreSQL's deadlock detector. A blocked
backend reports `wait_event_type = 'Lock'` with its wait's kind in
`pg_stat_activity`, and `pg_blocking_pids(pid)` returns the sessions
holding what it waits on (NULL in, NULL out) — read off the shared graph.
`pg_blocking_pids` rides the executor hook like the advisory locks; the
catalog-reader path cannot take a column argument, which is exactly what an
operator's query has.

#### Fixed

- A deadlock cycle through more than one lock kind is detected: one waiter
  gets PostgreSQL's `40P01` (fixes RP-048,
  `tests/backlog/test_rp_lock_waits.py::test_a_cycle_across_lock_kinds_is_a_deadlock`).
- `pg_locks`' transactionid and virtualxid rows, the ungranted
  transactionid row of a row waiter, `pg_stat_activity.wait_event`, and
  `pg_blocking_pids()` (fixes RP-047; its four gap tests in
  `tests/backlog/test_rust_postgres_gaps.py` are fixed-marked).
- The relation lock modes of row-locking and writing statements match
  PostgreSQL's (part of RP-047, same tests).

#### Added

- `secantus_pgplan::privileges::row_locked_tables` — the tables a SELECT's
  locking clause (`FOR UPDATE` / `FOR SHARE`, with `OF`) row-locks.
- `xids::running` and `snapshots::ask_pid` — reading other backends'
  assigned xids for the catalogs.
