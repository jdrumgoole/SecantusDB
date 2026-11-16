### Rust MongoDB server: `collMod` reports mongod's locks

A `collMod` of an existing collection now takes the locks mongod 8.2.11 takes:
the global lock in IS on its own, then one write round that holds the
collection in X twice, so its slow-query line and profile entry print mongod's
`locks` (`ReplicationStateTransition: {w: 1}`, `Global: {r: 1, w: 1}`,
`Collection: {W: 2}`).

#### Fixed

- `collMod`'s `locks` were the plain write chain taken once, with no
  replication state transition lock and no global IS.

Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_write_reports_mongods_locks_and_flow_control[collMod])
