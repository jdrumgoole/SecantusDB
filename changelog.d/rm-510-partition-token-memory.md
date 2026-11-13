### Rust MongoDB server: a partitioned window's `maxTotalMemoryUsageBytes` counts what mongod's iterator counts

`explain`'s `maxTotalMemoryUsageBytes` for a `$setWindowFields` with
`partitionBy` now matches mongod 8.2's. mongod's partition iterator replaces
its memory token by assigning a new one over the old, so its tracker counts
both for a moment: once while it reads the next partition's first document
(this partition's key, that document and its key), and once as it starts
that partition (the document in the cache and in the old token, and its key
twice).

#### Fixed

- Over three partitions of 40 small documents, a `range [-4, 4]` `$sum`
  reports 748 (was 764), a whole-partition `$sum` 2180 (was 2196), a running
  `$sum` 480 (was 464), an `$expMovingAvg` 432 (was 416), a `["unbounded",
  -1]` `$push` 756 (was 724) and `$integral` 1940 (was 1836), as mongod does.
  The server had counted both keys for the whole update after the read, and
  nothing at a partition's start.
- A removable window function (`$sum` over a bounded window, and the like)
  now counts its own size before its first answer in a partition, as
  mongod's executor sets it on construction and on reset, so a range window
  that reads past a short partition's end at its first document counts it.
  Fixes RM-510 (tests/backlog/test_rm_found_2026_11_10_rm404.py::test_a_partitioned_windows_total_memory;
  regression tests `tests/backlog/test_rm_found_2026_11_10_rm404.py::test_which_partition_key_each_moment_counts`
  and `secantus-core` `budget_tests::replacing_the_partition_token_counts_both`).
