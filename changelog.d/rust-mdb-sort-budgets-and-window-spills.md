### Blocking sorts hold mongod's budget and spill; `$setWindowFields` spills its partition

The Rust MongoDB server now runs every blocking sort as mongod 8.2's
external sorter does. A `$sort`, a `find` with a sort, `$bucketAuto`'s sort
and the sort mongod puts in front of a `$setWindowFields` each count their
keys and documents against the sort budget, measured in the sizes mongod
uses for that kind of sort. Past the budget a sort writes sorted runs to
disk and merges them, with the spill and merge counts mongod reports, or
refuses with 292 under `allowDiskUse: false`. `$setWindowFields` now
models mongod's partition cache for every window shape and spills it.

#### Fixed

- A blocking sort over its budget is 292 under `allowDiskUse: false`, at
  mongod 8.2.11's exact boundary: the query layer's sort (`find`, a
  pipeline's leading `$sort`), a `$sort` after a read, an `$addFields` or an
  `$unwind`, a sort SBE runs, and `$bucketAuto` (under
  `internalDocumentSourceBucketAutoMaxMemoryBytes`). Each was bisected
  against mongod over several document sizes. With disk allowed the sort
  spills and merges `max(budget / 64 KiB, 2)` runs at a time, as mongod
  does, and answers in the same order. Fixes RM-315
  (tests/backlog/test_rm_found_2026_10_20b.py::test_blocking_sort_memory_limit).
- `$setWindowFields` counts the documents its windows hold (a window that
  drops documents holds the one answered and those ahead of it; one with no
  upper end, the partition) and its functions' memory, and spills the cache
  when disk is allowed. It refuses with 5643011 or 5414201 where mongod does
  for whole-partition, `["unbounded", "current"]` and `$shift` windows.
- A profiled operation reports `hasSortStage`, `sortSpills`,
  `sortSpilledBytes`, `sortSpilledRecords` and `sortTotalDataSizeBytes`, and
  a `$group`'s `groupSpilledBytes` is mongod's memory estimate at each
  spill. `explain` reports a `SORT`'s, a `$sort`'s and a `$group`'s spill
  counters. `serverStatus` has `metrics.query.sort`, `.group`,
  `.setWindowFields` and `.bucketAuto`.
- SBE's `$concatArrays` counts each operand array as one value, as mongod
  does, and the classic accumulators count their own sizes as mongod's
  `explain` shows them.
- Reading a document no longer scans all of its bytes for the SQL server's
  system fields, only its top-level field names. That scan took a debug
  build 15 s to read 110 one-megabyte documents through a `$group` on
  `_id`; it now takes 0.1 s. (This is in `secantus-storage-adapter`, which
  the PostgreSQL server also links.)

#### Added

- `internalQueryMaxBlockingSortMemoryUsageBytes` and
  `internalDocumentSourceBucketAutoMaxMemoryBytes` are runtime parameters,
  with mongod's types and bounds; `explain`'s `serverParameters` and a
  `SORT`'s `memLimit` show the budgets in force.

Still open: spill files' on-disk sizes (RM-316), mongod's per-batch profile
accounting of a sort under a cursor stage (RM-317), some
`$setWindowFields` windows' boundaries (RM-318) and `explain`'s memory
counters (RM-319).
