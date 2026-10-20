### Rust MongoDB server: a journaled write survives a crash

A write acknowledged with `j: true`, or with `w: "majority"`, is now synced
to the journal before it is acknowledged. That includes the implicit default
write concern, `{w: "majority"}`. A background journal flusher syncs every
100 ms for the remaining writes. This is mongod 8.2.11's mechanism
(`waitUntilDurable`, `storage.journal.commitIntervalMs`).

#### Fixed

- Writes acknowledged with `j: true` were lost to a SIGKILL. `j` was echoed
  back but nothing synced the log, and only the 60-second checkpoint did.
  Measured before: 0 of 50 acknowledged inserts survived. After: 50 of 50
  for `j: true`, for the default write concern, and for `w: 1` writes 300 ms
  before the kill, five runs each.
- A store in fast test mode (`durable=false`) writes the log out without
  syncing it, so a killed process still keeps a journaled write.

#### Changed

- A write with the default write concern now waits for the sync, as on
  mongod. On macOS with a debug build, 2,000 single inserts took 4.5 ms each,
  against mongod's 5.0 ms. With `w: 1` they took 0.28 ms, against mongod's
  0.10 ms.

Fixes RM-186
(`tests/backlog/test_rm_found_2026_10_20.py::test_j_true_write_survives_sigkill`).
The fix is in the shared `secantus-storage` / `secantus-wt` crates, which
the Rust PostgreSQL server also links; that server already syncs every
commit.
