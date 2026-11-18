### An SBE `$group` decides when to spill, and counts what it spills, as mongod does

The Rust MongoDB server's SBE `$group` now checks its hash table the way
mongod 8.2's `HashAggStage` does. At checkpoints paced by how fast the
table grows, it multiplies the size of the row the last document went to by
the number of groups. It spills once that product reaches the budget. Each
spill's `groupSpilledBytes` is now the bytes of the records written: the
accumulator row as mongod serializes it, plus the type bits of the group
key. The value reported before was this server's own memory estimate.

#### Fixed

- `groupSpilledBytes` for an SBE `$group` showed the server's memory
  estimate (306900 where mongod 8.2.11 reports 312300 for 300 `$push`
  groups). Some shapes also spilled a different number of times than
  mongod's: a `$push` beside a `$sum`, a compound `_id` with `$addToSet`
  and `$maxN`, and a compound-sorted `$topN`.
- Fixes RM-612 (tests/backlog/test_rm_found_2026_11_13_rm325.py::test_an_sbe_group_spills_as_mongods),
  with the byte count now also compared in
  tests/mongod_live/test_aggregation_memory.py::test_an_sbe_group_spills_to_the_spill_engine
  and the layouts pinned in `secantus-core`'s `sbe_hashagg` unit tests.
