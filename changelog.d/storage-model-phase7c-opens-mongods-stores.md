### SecantusDB opens the data directory mongod wrote

`secantusd-mdb` now opens a data directory mongod 8.2 wrote and serves it:
its collections and indexes, their sizes, its oplog and its replica set,
under the set's own name. A change stream opened on mongod resumes on
SecantusDB from mongod's token, and mongod opens the directory again
afterwards and validates every collection. A directory mongod was killed on
is recovered as mongod's own restart recovers it: WiredTiger recovers its
journal, the oplog is truncated after `oplogTruncateAfterPoint`, and every
entry after the stable checkpoint is replayed, so SecantusDB serves what a
restarted mongod would, document for document. What it cannot serve yet (a
time-series collection, an index build mongod left unfinished, the index
kinds it keys differently, users and roles) is refused by name before
anything is written. The format file is written only once the open has
succeeded.

SecantusDB's own crash recovery now truncates the oplog after the point as
mongod's does, so, as on mongod, a write acknowledged with `{w: 1, j: false}`
in the last ~100 ms before a crash can be lost; a journaled or majority write
never is, and neither is any write a server started with `--sync-on-commit`
acknowledges. Three bugs found on the way are fixed: writes mongod made to a
SecantusDB directory were lost when SecantusDB opened it again, a collection
created shortly before a crash came back from recovery without its UUID, and
a transaction committed shortly before a crash came back without its
session's `config.transactions` row. The store format is unchanged (9).

#### Added

- `secantus-storage`: a store mongod wrote (`storage.bson`, no
  `secantus.format`) opens: `store_format::prepare` answers
  `StoreKind::Mongod`, and the new `adopt::reconcile` rebuilds the registry
  (collections, placements, index rows, views) from `_mdb_catalog` and
  `system.views` after refusing by name what this server cannot serve
  (`store_format::catalog_findings`). Proved by
  `tests/backlog/test_rm_storage_model.py::test_secantus_opens_a_mongod_store`,
  `::test_secantus_serves_the_phase_0b_store_mongod_wrote` and
  `::test_secantus_opens_a_killed_mongod_store`; RM-723 is closed.
- `secantus-server`: a store whose `local.system.replset` names another set
  is served as that set's member, under its name.
- `secantus-commands`: `configureFailPoint` honours
  `pauseJournalFlusherBeforeFlush` (under `--enable-test-commands`): the
  journal flusher stops moving the truncate point and syncing, and a
  journaled write waits for it, as on mongod.

#### Fixed

- Fixes RM-736: crash recovery truncates the oplog after
  `local.replset.oplogTruncateAfterPoint`, then clears it, before the replay
  (decided by Joe, 2026-11-22: "Truncate as mongod")
  (`crates/secantus-storage/tests/oplog_truncate_after_point.rs`,
  `tests/backlog/test_rm_storage_model.py::test_recovery_truncates_the_oplog_after_the_point_as_mongod`).
  A journaled write's wait now also waits for the oplog entries minted
  before its connection's last op to commit, as mongod's acknowledgement
  waits for its durable point to pass the client's last op
  (`ReplClientInfo::getLastOp`) with no hole before it, so the point it
  moves covers the write. It does not wait for entries other writers minted
  after it: a first build that did cost 8-30% at 4 writers on the droplet
  A/B, and the end of an operation's scope did not wake the waiters. Under `--sync-on-commit` every acknowledged write waits as a
  journaled one does, moving the point first
  (`tests/test_rust_binary_crash_durability.py::test_acknowledged_journaled_writes_survive_sigkill_mid_load[sync-on-commit-w1]`).
- Fixes RM-757: a store SecantusDB wrote, then mongod wrote to, kept mongod's
  writes when SecantusDB opened it again; before, a collection mongod made
  had its tables dropped as orphans and one mongod dropped was still served
  (`tests/backlog/test_rm_storage_model.py::test_secantus_keeps_what_mongod_wrote_to_its_store`).
- Fixes RM-758: a collection whose `create` crash recovery replays keeps the
  UUID its entry names; before, it had none, mongod refused the recovered
  store, and its next write minted a new one
  (`tests/backlog/test_rm_storage_model.py::test_a_collection_keeps_its_uuid_through_crash_recovery`).
- Fixes RM-760: crash recovery's replay of a committed transaction writes its
  session's `config.transactions` row (`state: "committed"`, the entry's
  timestamp and wall time), as mongod's does; found comparing the two
  servers' recoveries of one killed store
  (`tests/backlog/test_rm_storage_model.py::test_crash_recovery_keeps_a_transactions_session_row`).
- A batched delete's 5 ms batch time (`batchedDeletesTargetBatchTimeMS`,
  RM-609) starts at the batch's first delete, as mongod's `batchTimer`
  does; it counted the collection's index and option lookups too, so a slow
  machine split a batch of ten after its first delete
  (`tests/backlog/test_rm_219_apply_ops.py::test_oplog_batched_delete_is_apply_ops`).
