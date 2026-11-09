### Rust MongoDB server: a slow-query line says what execution admission control admitted

mongod's `Slow query` line counts the execution tickets an operation took, how
long it spent taking them, and the tickets it held too long. This server's line
now says the same: `queues` carries `execution` beside `ingress`, with mongod's
count for each command (2 for a `find`, 3 for an `insert`, 4 for a `drop`, `{}`
for a `ping`), `storage.timeWaitingMicros.storageExecutionMicros` is on the line
and the profile entry whenever a ticket was taken, and `delinquencyInfo` appears
when a ticket was held past `delinquentAcquisitionIntervalMillis`.

#### Fixed

- The line prints the operation's `queues.execution` admissions and
  `delinquencyInfo`, and the line and profile entry its
  `storageExecutionMicros`: the time each execution-ticket acquisition took,
  including a ticket that was free at once.
- The global IS the line reads the operation's statistics under is taken
  inside the operation's admission context, before the line is built, so its
  ticket counts as one more admission, as on mongod. Its `locks` are still the
  ones read before.

Fixes RM-355 in part (tests/backlog/test_rm_355_364.py::test_a_line_says_what_execution_control_admitted,
tests/backlog/test_rm_001_028.py::test_slow_query_line_says_where_the_time_went).
Pinned per operation by tests/mongod_live/test_slow_query_lines.py, which now
compares `queues` and `storage` with mongod's, and
tests/mongod_live/test_execution_admission.py::test_a_ticket_held_too_long_is_on_the_line.

What remains of RM-355 has a new gap test,
tests/backlog/test_rm_355_364.py::test_admission_control_is_reported_where_mongod_reports_it.
