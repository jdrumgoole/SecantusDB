### A standalone store keeps no stable timestamp, so mongod starts on it even after a kill

A `--standalone` store of `secantusd-mdb` (one with no replica-set
configuration) now takes the checkpoints a standalone `mongod` does: every
committed write, with no stable timestamp. `mongod` 8.2 started as a one-member
set on such a store, killed within a second or two of its last write, no longer
dies on fatal assertion 28560, and no longer on the invariant
`getOplogTruncateAfterPoint(opCtx).isNull()` either.

#### Changed

- Joe decided the faithful redesign (2026-11-25): on a store with no
  `local.system.replset`, the periodic and on-demand checkpoints are taken with
  `use_timestamp=false` and set no stable timestamp (the oldest timestamp still
  trails the clock). Each is preceded by one row, `replayFrom`, in a new
  unlogged table `secantus_replay_from` (written before the checkpoint starts,
  from a position already committed, so the checkpoint holds it atomically
  with the data). A crash's recovery replays the oplog after that position;
  the replay was already idempotent. A replica-set member's checkpoints,
  stable timestamp, truncate-after point and recovery are unchanged, and a
  store that gains a configuration (`replSetInitiate`) takes stable
  checkpoints from the next tick. The store format number does not change:
  the table is created when a store is opened, and older stores simply gain it.
- A standalone keeps `local.replset.oplogTruncateAfterPoint` null: a new
  store's genesis writes null (it wrote the initiating entry's timestamp), and
  the journal flusher and a journaled write's wait move it only once the store
  holds a replica-set configuration, as only a primary's do on mongod.

#### Fixed

- Fixes RM-805 (`tests/backlog/test_rm_storage_model.py::test_mongod_starts_on_a_standalone_store_killed_within_the_second`;
  the kill matrix is
  `tests/backlog/test_rm_storage_model.py::test_a_killed_standalone_store_keeps_every_acknowledged_write_once`).
- Fixes RM-806 (`tests/backlog/test_rm_storage_model.py::test_mongod_starts_as_primary_on_a_standalone_store_killed_long_ago`).
