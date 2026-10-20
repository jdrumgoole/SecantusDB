### Rust storage: a failed close reaches the exit status

#### Fixed

- `Storage::close()` now returns the first failure: the stable checkpoint,
  persisting the oplog meta row, or the final checkpoint. Each failure is
  still logged. `close()` runs once, and the `Drop` after it does nothing.
- `secantusd-rs` closes its store on shutdown and exits non-zero if the
  close failed. Before, a daemon whose last checkpoint failed printed a line
  and exited 0.
- The WiredTiger wrapper's `Connection`, `Session` and `Cursor` drops log a
  failed `close` instead of discarding its return code.

Fixes RM-189. Its Verify line is `none` (it needs a checkpoint failure
injected at shutdown); `close_is_idempotent_and_reopens` covers the success
path. The change is in the shared `secantus-storage` / `secantus-wt` crates,
which the Rust PostgreSQL server also links; that server still closes
through `Drop`.
