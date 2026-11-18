### An SBE `$group` spills to the spill engine, under a budget you can set

The Rust MongoDB server now takes mongod 8.2's SBE hash-aggregation budget,
`internalQuerySlotBasedExecutionHashAggApproxMemoryUseInBytesBeforeSpill`,
at startup (`--setParameter`) and at runtime (`setParameter`, `getParameter`),
as mongod 8.2.11 does. An SBE `$group` over it spills into one table of the
server's spill WiredTiger engine and reads its runs back from it, as
mongod's `HashAggStage` does, instead of a file of its own.

#### Fixed

- The SBE `$group` budget was always 100 MiB, and the startup parameter was
  refused as unknown. It is now a long above 0, refused at startup in
  mongod's words. With it at 50000, a `$push` of 300 1000-byte strings
  spills 7 times, 300 records, into a 4096-byte spill table, the same
  counts as mongod 8.2.11.
- Trims RM-612 (tests/mongod_live/test_aggregation_memory.py::test_an_sbe_group_spills_to_the_spill_engine):
  `groupSpilledBytes`, and how often a group with a large key spills, are
  still this server's estimate and stay open under RM-612.
