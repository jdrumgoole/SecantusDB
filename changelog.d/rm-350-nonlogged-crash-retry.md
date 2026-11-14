### A retryable write retried after a crash under `--data-nonlogged` is not applied again

A data-nonlogged store writes a session's `config.transactions` row and a
retryable `findAndModify`'s image without an oplog entry, and recovers its
data tables from the oplog after a crash, so a SIGKILL lost both: a retried
insert failed on a duplicate key and a retried `findAndModify` changed its
document a second time. Crash recovery now rebuilds the row and the image as
it replays each retryable write's entry, as mongod 8.2.11's
`SessionUpdateTracker` does, and both retries are answered as after a clean
restart. The change is in `secantus-storage`, which the Rust PostgreSQL
server links too.

#### Fixed

- Fixes RM-350 (tests/backlog/test_rm_350_retry_after_restart.py::test_a_retry_after_a_crash_under_data_nonlogged_applies_nothing_twice)
