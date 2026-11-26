### A store's tables follow the mode the server starts in, as mongod's `setTableLogging`

`secantusd-mdb --standalone` now keeps its collections and indexes in the
journal, as a standalone `mongod` does, and a replica-set member's are
unlogged. A killed standalone store handed straight to `mongod` (standalone or
`--replSet`) holds every journaled write.

#### Changed

- Joe decided the faithful option (2026-11-26). Measured on `mongod` 8.2.11, a
  store opened without `--replSet` has every user collection and index logged
  (`admin.system.version` too), and one opened with it has them unlogged; the
  `local` collections and the oplog stay logged either way. `secantusd-mdb`
  now does the same at open: it opens the store in the mode it was made in
  (a member's recovery replays the oplog), closes it, alters each table's
  `log=(enabled=...)` with `WT_SESSION::alter` ("Changing table logging
  settings" on its log), checkpoints, and opens it again in the mode asked
  for. `StorageOptions::replica_set` carries the mode; `--data-logged` and
  `--data-nonlogged` still override it. A standalone's data is therefore
  recovered by WiredTiger's log, and the standalone-only checkpoint and replay
  machinery of RM-805 serves only a store opened without a mode. No format
  number changes.

#### Fixed

- Fixes RM-808 (`tests/backlog/test_rm_storage_model.py::test_mongod_keeps_the_writes_of_a_killed_standalone_store`;
  the hand-offs are
  `test_a_killed_store_is_handed_straight_to_mongod_with_every_write`, the
  mode switches `test_a_store_switched_between_standalone_and_member_keeps_every_write`
  and `test_secantus_opens_a_standalone_mongod_store_in_both_modes`).
