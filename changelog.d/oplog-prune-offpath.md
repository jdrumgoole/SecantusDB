### The oplog prune stops taxing every write — +25% single-writer, +62% at eight writers

Phase-0 profiling of the concurrency-parity program (Finding 12) caught the
Rust server's opportunistic oplog prune consuming ~36% of the sustained
write path: once a workload passes the 100k-entry oplog cap — about four
seconds into any sustained run — every sweep re-read the full 8 KiB value
of every doomed row through the shard merge, copying ~8 MB per sweep just
to learn which seqs to delete, on the writer's own thread. The sweep now
walks keys only, peeking a row's timestamp just in the retention tail
beyond the cap excess, and the emit path stops re-running WiredTiger's
schema-locked `create` for its oplog shard on every batch (a
first-touch bitmask remembers what exists). Measured on the Finding-12
baseline rig: sync single-writer 25.4k → 31.6k docs/s (+25%), eight
writers 41.5k → 67.3k (+62%), lifting durable-path scaling from 1.65× to
2.13× and oplog retention from 22% to 36% of the no-oplog ceiling.

The slice also closes the `startAtOperationTime` residual recorded by the
visibility-point fix: `find_seq_for_ts` no longer finalises a resume
position past a minted-but-uncommitted oplog entry whose timestamp
qualifies — it waits (bounded) for the in-flight window to drain past its
committed-view answer and rescans, so a transaction committing mid-open
surfaces the earlier event instead of losing it.

#### Fixed

- Rust server: `startAtOperationTime` can no longer skip an event whose
  oplog entry was minted inside a still-open transaction (bounded wait on
  the in-flight window; falls back to the committed view at the deadline —
  today's behaviour — only for long-open transactions).

#### Changed

- Rust server: the opportunistic oplog prune identifies doomed rows with a
  key-only shard merge (values peeked only for the retention tail), and
  oplog shard tables are created on first touch instead of per-batch.
