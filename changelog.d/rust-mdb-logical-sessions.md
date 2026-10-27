### Rust MongoDB server: mongod's logical session cache, its reaper, and a retryable bulkWrite's oplog

The Rust MongoDB server now keeps sessions the way a mongod replica-set
primary does. Every session a command names is written to
`config.system.sessions` by a refresh every `logicalSessionRefreshMillis`
(five minutes), an ended session's record is deleted by the next refresh,
and a record nobody refreshes expires after `localLogicalSessionTimeoutMinutes`.
A retryable write or a committed transaction leaves its session's row in
`config.transactions`, and the transaction reaper deletes a session's row and
its `findAndModify` image once its record has gone, as logged deletes, so a
tool that reads the oplog sees the entries mongod 8.2.11 writes.

A retryable `bulkWrite` now logs its writes under its session, as the single
write commands already did: each op under its statement id with the
`prevOpTime` chain, and consecutive inserts into one namespace as one
`applyOps`, the way mongod batches them.

#### Added

- `crates/secantus-commands` (`logical_sessions`): the logical session cache's
  refresh (upserts each session used since the last refresh, deletes ended
  sessions' records, closes the cursors of ended or expired sessions) and the
  transaction reaper (deletes the `config.image_collection` image and then the
  `config.transactions` row of every session without a record, and forgets
  its retryable writes, so a retry runs again). The server runs both every
  `logicalSessionRefreshMillis` unless `disableLogicalSessionCacheRefresh`.
- The test commands `refreshLogicalSessionCacheNow` and
  `reapLogicalSessionCacheNow`, behind `--enable-test-commands` as on mongod.
- `config.transactions` rows (`{_id, txnNum, lastWriteOpTime, lastWriteDate}`,
  `state: "committed"` for a transaction), written without an oplog entry once
  a retryable write or transaction of the session logged a write.
  `config.transactions` (with its `parent_lsid` index) and
  `config.image_collection` are created when a replica-set member starts, as
  mongod creates them on step-up.
- `secantusd-rs --setParameter NAME=VALUE` for the four startup-only session
  parameters (`localLogicalSessionTimeoutMinutes`,
  `logicalSessionRefreshMillis`, `disableLogicalSessionCacheRefresh`,
  `TransactionRecordMinimumLifetimeMinutes`), refused as mongod refuses a bad
  one; `getParameter` reports them, `hello`'s `logicalSessionTimeoutMinutes`
  and `lsidTTLIndex`'s TTL follow the timeout.
- `serverStatus.logicalSessionRecordCache` reports both jobs in mongod's
  fields.

#### Fixed

- Fixes RM-224 (tests/backlog/test_rm_219_apply_ops.py::test_bulk_write_entries_carry_their_session):
  a retryable `bulkWrite`'s oplog entries carry `lsid`, `txnNumber`, `stmtId`
  and `prevOpTime`; consecutive inserts into one namespace are one `applyOps`
  (`multiOpType: 1` when retryable) in a retryable or plain `bulkWrite`.
  Compared live by tests/mongod_live/test_logical_sessions.py::test_a_retryable_bulk_write_logs_its_session.
- A session's `findAndModify` image is deleted by the reaper once the session
  expires (tests/mongod_live/test_logical_sessions.py::test_the_reaper_deletes_an_expired_sessions_state).
- `$listSessions` lists the records of `config.system.sessions`, and
  `$listLocalSessions` the sessions used since the last refresh, as on mongod
  8.2.11; both used to list every session seen in the last 30 minutes.
- `config.image_collection` is no longer created, with a logged `create`, the
  first time a retryable `findAndModify` keeps an image.
- A retryable write's (and a committed transaction's) `operationTime` is the
  time of its own oplog entry, though keeping the session's row moves the
  clock on; a change stream started at it sees the write
  (crates/secantus-storage-adapter/tests/logical_sessions_wt.rs).

Filed RM-350: a retryable write retried after a restart is applied again
(tests/backlog/test_rm_350_retry_after_restart.py).
