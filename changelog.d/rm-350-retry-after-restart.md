### A retryable write retried after a restart is answered, not applied again

A driver retries a write exactly when it lost the connection, and a server
restart is the commonest way to lose one. The Rust MongoDB server kept what
each retryable write had done only in memory, so a retry that reached a
restarted server ran again: a retried insert failed on a duplicate key, and a
retried `findAndModify` changed its document a second time.

Now a retryable write on a session the server holds nothing for is answered
the way mongod 8.2 answers it: the server reads the session's
`config.transactions` row, the oplog entries of its transaction number and,
for a `findAndModify`, the image it kept in `config.image_collection`, and
answers the statements that already ran with `retriedStmtIds` /
`retriedStmtId` instead of running them again. An older transaction number
is refused as `TransactionTooOld`, as before the restart.

#### Fixed

- `secantus-commands` (`session::retryable_history`,
  `TransactionRegistry::restore_retryable`): a retryable insert, update,
  upsert, delete or `findAndModify` retried after a clean restart is answered
  from storage, as on mongod 8.2.11. Part of RM-350
  (tests/backlog/test_rm_350_retry_after_restart.py::test_a_retry_after_a_restart_applies_nothing_twice,
  tests/backlog/test_rm_350_retry_after_restart.py::test_each_kind_of_retry_after_a_restart);
  a crash under `--data-nonlogged`, which loses the row and the image, stays
  open under it.
