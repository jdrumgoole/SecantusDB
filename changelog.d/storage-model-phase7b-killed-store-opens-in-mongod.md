### mongod 8.2 recovers a store SecantusDB was killed on

A data directory `secantusd-mdb` was SIGKILLed on, mid-write, is now one
mongod 8.2.11 starts on as the replica set's one member, recovers and serves:
it truncates the oplog after `oplogTruncateAfterPoint`, replays the oplog from
the checkpoint's stable timestamp, steps up to primary, holds every document
acknowledged with `j: true`, and validates every collection. Before, the
recovery timestamp a crash left could be past the oplog's last entry, and
mongod aborted on such a store.

The cause was which timestamp each write committed at. mongod's stable
timestamp is always an oplog entry's, and every write it timestamps is at an
entry's timestamp: a retryable write's `config.transactions` row and retry
image share the write's entry, a collection drop's catalog changes the drop's,
and what mongod logs no entry for (the profiler, `replSetResizeOplog`, an
index build's keys, its startup writes) it writes untimestamped. SecantusDB
now does the same, and its stable timestamp is the newest committed entry
rather than the newest timestamp it minted. The truncate point moves as
mongod's journal flusher moves it, every 100 ms and before a journaled
write's sync, so mongod's recovery keeps every write acknowledged as
journaled (as on mongod, a `w: 1` write of the last 100 ms can be truncated).

Opening a store mongod wrote is still refused, but the refusal now names what
in it this server could not serve: a replica-set configuration of more than
one member, an index of a kind it does not key as mongod does, a collection or
index option it does not keep, another feature compatibility version, users
and roles. The store format is unchanged (9).

#### Fixed

- `secantus-storage`: the stable timestamp, and so a checkpoint's recovery
  timestamp, is the newest oplog entry at or below the all-committed point,
  never a cluster-time mint or a write without an entry. Fixes RM-746
  (`tests/backlog/test_rm_storage_model.py::test_mongod_opens_a_killed_secantus_store`).
- `secantus-storage`: `local.replset.oplogTruncateAfterPoint` names the newest
  entry at or below the all-committed point and is moved before every journal
  sync (the 100 ms flusher, `j: true` / `w: "majority"` waits, stable
  checkpoints), as mongod's is. Fixes the refresh half of RM-736
  (`tests/backlog/test_rm_storage_model.py::test_the_truncate_after_point_follows_the_oplog`);
  whether this server's own recovery truncates after it stays a decision.
- `secantus-storage`: a write with no oplog entry commits untimestamped, or,
  inside an operation (`Storage::op_scope`: a retryable write, a transaction's
  commit, a drop, a rename, a view's DDL, an index build's steps), at the
  timestamp of the operation's entry. The commit-stamp reservation
  (`reserve_stamp`) is gone. The oplog entries a transaction of their own
  writes commit at their own timestamp.
- `secantus-storage`: a statement transaction nested in another's (an index
  build's `config.system.indexBuilds` row) no longer deregisters the outer
  statement's oplog entries before the outer one commits.
- `secantus-commands`: `system.profile` writes are unreplicated.

#### Changed

- `secantus-storage`: a mongod store is refused naming what this server could
  not serve in it (read with WiredTiger opened read-only); a killed mongod's
  store, whose log needs recovery, is refused without the list. Partly closes
  RM-723 (`tests/backlog/test_rm_storage_model.py::test_secantus_refuses_a_set_of_two_members_by_name`,
  `::test_secantus_refuses_an_index_kind_it_does_not_serve_by_name`).
