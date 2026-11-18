### Rust MongoDB server: `$out` counts a replica set's locks

An `$out` on a replica set's primary now reports mongod 8.2.11's `locks`:
it checks the pipeline and its target under four global IS, each with the
replication state transition lock, and takes the transition lock alone once
more for each exclusive round after the first. It took a standalone's six
global IS without the transition lock.

#### Fixed

- `$out`'s line on a replica set read `ReplicationStateTransition: {w: 5}`
  and global `r: 7`, where mongod reads `{w: 10}` and `r: 5` (12 and 5
  replacing its target). A standalone keeps its own rounds.

Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_out_reports_a_replica_sets_locks)
