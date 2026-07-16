### Concurrent writers: commit-time conflicts retry instead of erroring

Under concurrent writers, a WiredTiger batch transaction can be marked
rollback-only by a competitor after its last operation succeeded; the
conflict then surfaces at `commit_transaction` as a bare EINVAL with no
`WT_ROLLBACK` marker, which escaped the write-conflict retry wrapper and
reached clients as a generic `InternalError` (code 1). Found by the new
three-server `bench.concurrency` harness. Commit failures now map to the
retryable `WriteConflictError` when WiredTiger reports a rollback reason or
the documented rollback-required EINVAL shape; commit failures that are
neither (I/O errors, panics) stay loud, per the never-swallow rule. The
remaining structural contention (every batch transaction updates the shared
oplog-meta row, so writers on different collections still conflict) is
recorded in `tasks/backlog.md` for the WT concurrency plan.

#### Fixed

- `storage._commit_batch_transaction`: commit-time rollback-required
  failures become `WriteConflictError` (retried outside user transactions;
  mongod's statement-time `WriteConflict` inside them) instead of
  `InternalError`.
