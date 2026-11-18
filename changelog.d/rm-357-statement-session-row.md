### A retryable update or delete keeps its session's row per statement

A retryable `update` or `delete` statement now takes its own lock round as it
runs and keeps its session's `config.transactions` row under it before its
line is written, as mongod 8.2.11 does, so each statement's `locks` and
`flowControl` read as mongod's.

#### Fixed

- A retryable update or delete statement's line now counts the round its
  session's row is written in, and a statement no longer counts the rounds
  of the statements after it.

Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_retryable_statement_keeps_its_session_row_under_its_locks)
