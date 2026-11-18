### A retryable update or delete batch that fails part-way keeps what its applied statements did

A retryable `update` or `delete` batch whose operation was interrupted as the next statement took its locks (a `killSessions` landing between two statements) answered 11601 but dropped the records of the statements it had already committed. A retry of the same `lsid` / `txnNumber` then ran those statements again, applying `$inc` a second time and reporting no `retriedStmtIds`. The records of the statements that wrote are now kept whatever the batch answered, so the retry answers for them and runs only the rest, as mongod 8.2.11 does.

#### Fixed

- `crates/secantus-commands/src/session.rs`: `retry_batch` returns the applied statements' records with a failed reply too. Test: `a_batch_that_fails_part_way_keeps_the_records_of_its_applied_statements`; the live comparison is `tests/mongod_live/test_interrupts.py::test_a_killed_batch_keeps_the_statements_before`.
