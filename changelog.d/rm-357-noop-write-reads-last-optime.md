### A write that logged nothing reads the system's latest optime

On a replica-set member, mongod's command entry point moves a client's last
op to the system's latest optime before the write concern waits, when the
command took the global lock in a write mode and left the last op where it
found it (`setLastOpToSystemLastOpTime`). That read takes the replication
state transition lock and the global lock in IS. The Rust MongoDB server
already did this for `insert`, `update` and `delete`, and now does it for
every command that waits for a write concern.

#### Fixed

- A `create` of a collection that already exists, a `drop` of one that is
  not there, a `dropIndexes` of an index that is not there, and a `$merge`
  that changes nothing showed one transition IX and one global IS fewer in
  their `Slow query` line's `locks` than mongod 8.2.11's. They now match.
- RM-185's lock comparison, which measures against a standalone mongod, now
  runs this server as a standalone (`--standalone`) too, because a
  standalone makes none of these replica-set reads.
- RM-357 stays open for the remaining writes whose locks differ.

#### Found

- RM-551: `secantusd-rs --standalone` keeps a replica set's write concern.
  A write with no write concern waits for the journal as `w: "majority"`
  does, and `setDefaultRWConcern` and `getDefaultRWConcern` answer. mongod's
  standalone implies `w: 1` and refuses both commands with 51300.

Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_write_reports_mongods_locks_and_flow_control)
