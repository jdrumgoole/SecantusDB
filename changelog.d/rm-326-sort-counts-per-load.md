### A spilled sort is counted the way mongod's cursor stage counts it

When the stages after a pipeline's sort read it through mongod's cursor stage, mongod adds the sort's counts again for every buffer it loads from the query layer. A `$setWindowFields` over 300 documents whose sort spilled 13 times is therefore profiled with `sortSpills: 52`, and a first batch of 101 documents behind an `$addFields` with 39. The Rust MongoDB server now reports the same numbers in the profile and the slow-query line. It works out how many buffers mongod loads, of 32 documents at first and doubling, from the batch the client asked for. That count includes the `$skip` and `$limit` mongod moves into the query layer, and the 4 MB a buffer stops at. `metrics.query.sort.spillToDisk` now counts what mongod counts: one for each 64 KB chunk a sorter writes to its spill file, `$bucketAuto`'s sorter included.

#### Fixed

- `secantus-core` (`aggregate`, `memlimit`, `sorter`), `secantus-commands` (`aggregate`, `agg_executor`): the profile's and the slow-query line's `sortSpills`, `sortSpilledRecords`, `sortSpilledBytes`, `sortSpilledDataStorageSize` and `sortTotalDataSizeBytes` count a query-layer sort once per cursor-stage load, as mongod 8.2.11 does. `spillToDisk` counts sorter file chunks, and `$bucketAuto`'s spills now count in `metrics.query.sort` too.
- Fixes RM-326 in part (tests/backlog/test_rm_found_2026_10_26.py::test_sort_profile_per_batch, tests/backlog/test_rm_326_sort_counts.py::test_sort_counts_per_load). RM-326 stays open for stages whose reads depend on the documents (`$unwind`, `$match`, a bounded or partitioned window): `test_sort_counts_per_load_through_stages_that_filter`.
- Regression tests: `agg_executor::tests::the_cursor_stage_loads_as_mongod_does`, `agg_executor::tests::reads_that_depend_on_the_documents_are_not_followed`.
- Filed RM-400: a `getMore` reports none of its cursor's sort counts, and `totalKeysSorted` / `totalBytesSorted` count each sort once (`test_rm_326_sort_counts.py::test_get_more_sort_counts`).
- Filed RM-398: an SBE sort counts its bytes in `totalBytesSorted`, which mongod's does not (`test_rm_326_sort_counts.py::test_sbe_sort_bytes`).
- Filed RM-399: a sort with a limit does not spill as mongod's top-k sorter does (`test_rm_326_sort_counts.py::test_top_k_sort_spills`).
