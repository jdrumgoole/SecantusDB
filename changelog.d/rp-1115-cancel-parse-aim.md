### The prepared-Parse cancel test aims inside the Parse it measures

RP-215's regression test cancelled a prepared statement's Parse at fixed
times (0.5, 1.5 and 2.5 s) and once answered `ok` on a CI runner. Measured,
that runner's Parse cycle ended at about 2.5 s, so that cancel landed while
the backend was idle between the Parse's ReadyForQuery and the Bind. There
PostgreSQL 15.20 drops a cancel too, as `secantusd-pg` does (probed: a
cancel sent between a prepare and its execute leaves `pg_sleep(1)` to run
on both). The server's answer was faithful; the test's aim was not.

#### Fixed

- The test now times a Parse of the same shape first and cancels at 20%,
  45% and 70% of it (and at least 0.1 s before its end), so every cancel
  lands inside the Parse on any machine. A fast machine's 2.5 s cancel had
  landed in the Execute and tested no Parse at all. With the describe
  pass's cancellation points removed, the `[0.2]` and `[0.45]` cases fail,
  so it still catches RP-215. A miss now prints the server's cancel trace.
- Filed RP-1116, found while tracing it: a busy backend's held statistics
  are published mid-statement, because the first-message branch of
  `begin_message_cycle` never runs.

Fixes RP-1115 (tests/backlog/test_rp_batch53.py::test_a_cancel_during_a_prepared_statements_parse_is_honoured)
