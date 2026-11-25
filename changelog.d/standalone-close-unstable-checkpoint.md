### A standalone store's clean close takes an unstable checkpoint, so mongod starts on it

`mongod` 8.2 started as a one-member set on a `--standalone` store of
`secantusd-mdb` died on a fatal assertion (28560) when the store's newest
oplog entry was in the wall clock's current second or the next.

#### Fixed

- A `--standalone` store (no `local.system.replset` configuration) now ends
  a clean shutdown with a checkpoint of every update and no stable
  timestamp, as a `mongod` standalone's does. Before, the checkpoint kept
  the stable timestamp at the newest oplog entry, and `mongod`'s
  `replSetInitiate`, which commits at the current second with increment 1,
  was refused by WiredTiger ("commit timestamp (T, 1) must be after the
  stable timestamp (T, n)"). A reopened store's clock runs a second ahead
  of the wall clock, so a hand-off within a second or two of a second life's
  last write failed about three times in four; `reIndex` was not the cause.
  The next open of such a store replays the oplog from the position its
  close marker names, not from the oplog's first entry. A replica-set
  persona's store is unchanged: `mongod` opens it as the set's member and
  resumes its clock from the oplog.
- Fixes RM-797 (`tests/backlog/test_rm_storage_model.py::test_mongod_starts_on_a_standalone_store_closed_within_the_second`,
  `crates/secantus-storage/tests/lifecycle.rs::a_standalone_close_records_no_stable_timestamp`).
