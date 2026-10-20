### Rust MongoDB server: locks are granted first come first served

#### Fixed

- **A `drop` waiting on live writers is no longer overtaken by every
  writer after it** (fixes RM-138). The lock manager keeps a queue per
  resource and grants it in order, as mongod's lock head grants its
  conflict queue. A request that finds others waiting waits behind them.
  An owner that already holds the resource converts in place, so a
  transaction never queues behind a request waiting on it. A transaction's
  insert behind a waiting `drop` now times out with 24 LockTimeout, as on
  mongod 8.2.11, where it used to go ahead. Tests:
  `tests/backlog/test_rm_transaction_locks.py::test_a_waiting_drop_is_not_overtaken`
  and `lock_manager::tests::a_waiting_exclusive_request_is_not_overtaken`.
