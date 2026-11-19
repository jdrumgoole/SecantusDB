### Measuring mongod's on-disk layout, the first step to sharing it

SecantusDB is moving its stores to the layout mongod itself writes, so that
mongod 8.2 can open a SecantusDB store and SecantusDB can open mongod's. This
first step changes no server: it measures the target. A new probe records a
real mongod 8.2.11 store table by table, and the measurement settled the
largest open question: the WiredTiger SecantusDB vendors can read and write
mongod 8.2's files once it is built the way mongod builds it, so no
WiredTiger upgrade is needed.

#### Added

- `tools/probes/mongod_store_layout.py`: runs a fixed scenario against mongod
  (or, with `--secantusd`, against `secantusd-mdb`) and records every table,
  its creation config, the `_mdb_catalog` and `sizeStorer` rows, and each
  index's raw KeyString keys. Captures in
  `tests/fixtures/storage_model/`.
- Backlog items RM-723 (the layout, both directions), RM-724 (no `shutdown`
  command) and RP-1074 (the PostgreSQL server's store), each with gap tests.
- CI's PostgreSQL backlog shard installs mongod 8.2 for RP-1074's tests.

#### Changed

- `tasks/mongod-storage-model-plan.md`: the measured target layout and the
  phase 0 results.
