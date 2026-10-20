### Rust MongoDB server: a transaction waiting for a lock no longer stops the server

#### Fixed

- **A server-wide hang** (RM-201). A transaction statement waiting for
  another transaction's lock held its own transaction's mutex for the whole
  wait. A second command on that session then waited for that mutex while
  holding the session registry, and every other command needs the registry,
  as does the accept loop when it reaps expired transactions. The server
  stopped answering, new connections included. Found by
  `tools/probes/transactions.py`: every scenario after the first two
  transactions creating one collection timed out.
  - The statement now lets its guard go before it waits.
  - The registry no longer locks a transaction while it is held, which also
    removes a lock-order inversion with `forget_started`.
  The probe runs 267 scenarios to the end, with 7 differing.
- **A transaction's lock request gives up after
  `maxTransactionLockRequestTimeoutMillis`** (5 ms, settable, -1 for no
  limit). It answers 24 LockTimeout, `Unable to acquire IX lock on
  '{Collection : db.c}' within 5ms...`, labelled TransientTransactionError,
  as mongod 8.2.11 does. Before, it waited for the other transaction to end.
- Tests: `tests/backlog/test_rm_transaction_locks.py::test_a_transaction_waiting_on_a_lock_leaves_the_server_answering`
  and `::test_a_transaction_lock_request_times_out`. RM-202 records the
  lock-manager differences that remain.
