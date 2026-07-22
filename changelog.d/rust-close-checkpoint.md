### Rust server: checkpoint on clean shutdown

The Rust storage engine now forces a WiredTiger checkpoint when it closes
cleanly, matching the Python server's long-standing behaviour. WiredTiger's
connection close does not implicitly checkpoint while the journal is enabled, so
previously a clean shutdown of the Rust server left the write-ahead log
un-truncated — the next startup recovered correctly, but by replaying the entire
retained log rather than resuming from a bounded checkpoint. Data was never at
risk (the journal was always on), but reopen did more work than it needed to and
the two servers diverged on their clean-shutdown durability semantics.

The engine now mirrors the Python server exactly: on a graceful stop it drains
all connection threads, persists its oplog bookkeeping, and takes a final
checkpoint that bounds recovery time and truncates the log. A close-time
`durable` flag — resolved from the same `SECANTUS_FORCE_DURABLE` /
`SECANTUS_TEST_FAST_STORAGE` environment controls the Python server already
honours — governs the checkpoint, so the production daemon is fully durable
while the fast test path stays fast.

#### Fixed

- `secantus-storage` (Rust): `Storage`'s close (`Drop`) now checkpoints when
  `durable` is set, mirroring Python `Storage.close`; the flag is resolved on
  open via `resolve_durable` with Python's precedence. Adds
  `Storage::open_with_config_durable` for an explicit override.
