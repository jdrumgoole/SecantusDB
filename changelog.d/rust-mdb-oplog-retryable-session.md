### Rust MongoDB server: retryable writes carry their session in `local.oplog.rs`

#### Fixed

- A retryable `insert`, `update` or `delete` now shows in
  `local.oplog.rs` as mongod 8.2.11 writes it:
  - `lsid` and `txnNumber` first, the statement's `stmtId`, and a
    `prevOpTime` that chains the session's entries for one transaction
    number;
  - an insert group carries `multiOpType: 1`, with a `stmtId` on each
    insert;
  - a multi-document update or delete carries no session, as on mongod.
- The statement ids are the ones the command's statements had, including
  when a retry re-runs only some of them.

RM-224 part (`tests/backlog/test_rm_219_apply_ops.py::test_oplog_retryable_writes_carry_their_session`).
