### A spilled sort is counted per load behind stages that filter or look ahead

mongod's cursor stage counts a query-layer sort's spills again at every buffer
it loads for a batch. How many buffers it loads depends on how much of the
sorted input the later stages read. An `$unwind` reads up to the document its
output came from, and so does a `$match`. A `$setWindowFields` reads one
document past the one it answers, as far ahead as its window reaches, and to
the end of a partition for a whole-partition function. Behind those stages
the Rust MongoDB server now records what each one read and loads the same
buffers mongod does. Its profile entry reports the same `sortSpills`,
`sortSpilledRecords`, `sortSpilledBytes` and `sortTotalDataSizeBytes` as
mongod. Before, it counted the sort once.

#### Fixed

- `secantus-core` / `secantus-commands`: behind a query-layer sort that spilled,
  `$match`, `$unwind`, `$redact` and `$setWindowFields` (with the per-document
  stages fused into it) record how many input documents they had read for
  each output. The window's count comes from `reach()` and its partition
  boundaries. `cursor_loads` uses those counts to work out how many buffers
  the cursor stage loads. Measured against mongod 8.2.11 at 10 batch sizes for
  a running total, a `[-1, 2]` window, a partitioned total, a partitioned
  `$rank`, an `$unwind` of arrays and a `$match`.
  Fixes RM-326 (tests/backlog/test_rm_326_sort_counts.py::test_sort_counts_per_load_through_stages_that_filter;
  tests/mongod_live/test_sort_counts_per_load.py::test_sort_counted_per_load_the_stages_read)
