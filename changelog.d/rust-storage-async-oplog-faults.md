### Rust storage: the opt-in async oplog reports what it loses

#### Fixed

Each of these was a storage error turned into silence. With `--oplog-async`,
an entry the oplog fails to keep is now logged and recorded, and
`Storage::close()` returns it, so `secantusd-rs` exits non-zero:
- a batch whose final retry at shutdown fails;
- a drainer that cannot open a session;
- an entry that cannot be encoded, which is still stored empty so the
  sequence stays contiguous;
- entries handed to a drainer that is gone.

A drainer that panicked is reported too; its `join` result used to be
ignored.

Fixes RM-193 (Verify: none, since it needs a failing write injected into
the drainer). `close_reports_an_async_oplog_fault` checks that a recorded
fault is what `close` answers. The change is in the shared
`secantus-storage` crate.
