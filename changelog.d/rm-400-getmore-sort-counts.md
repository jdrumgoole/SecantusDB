### A cursor's getMore reports what its plan has sorted so far

mongod reports a cursor's plan on every operation that reads it. Each `getMore` is profiled with the sort and group counts its cursor has built up so far, so after an `aggregate` reports `sortSpills: 52`, its `getMore` reports 52 too. When an operation ends, mongod adds its `keysSorted` and `sortTotalDataSizeBytes` to `metrics.query.sort.totalKeysSorted` and `totalBytesSorted`, whether or not the sort spilled. A sort is therefore counted again at every `getMore`, and again at every buffer the cursor stage loads from it. An `explain` counts nothing. The Rust MongoDB server now does all of this. A pipeline that blocks, such as a `$sort` or a `$group`, reads all of its documents for the first batch. The `getMore`s of such a pipeline's cursor now name the `aggregate` that opened it in `originatingCommand`, and repeat its plan summary and `hasSortStage`, as a streaming cursor's already did.

#### Fixed

- `secantus-core` (`memlimit`, `aggregate`) and `secantus-commands` (`cursors`, `aggregate`, `lib`): a cursor keeps what its operations reported and its cursor-stage load state. A `getMore` reports those counts plus its own work, and the loads its batch adds. `metrics.query.sort` counts an operation's keys and bytes when it ends, and counts nothing for `explain`. A query-layer sort is counted once per cursor-stage load even when it did not spill (6900 keys for a sort of 300 read in batches of 50, as on mongod 8.2.11).
- Fixes RM-400 (tests/backlog/test_rm_326_sort_counts.py::test_get_more_sort_counts).
- Regression tests: `test_rm_326_sort_counts.py::test_sort_counted_per_operation_without_a_spill`, `test_rm_326_sort_counts.py::test_get_more_group_spills`.
- Filed RM-503: a sort on a descending or tied key spills a file of a different size from mongod's (`test_rm_326_sort_counts.py::test_descending_and_tied_spill_storage_size`).
