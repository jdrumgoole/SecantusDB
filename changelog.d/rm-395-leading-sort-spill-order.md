### A pipeline's leading `$sort` spills the runs mongod spills

When a pipeline's first `$sort` is a blocking sort that runs past its memory budget, mongod's `SORT` stage takes documents in the order its plan reads them -- a collection scan's, for a plain collection -- and writes each run sorted. The Rust MongoDB server counted that sort over the read's output, already in sorted order, so the spill file held other runs: the same spills, records and bytes, but a different `sortSpilledDataStorageSize` (130528 bytes against mongod 8.2's 130545 for 300 documents sorted by `{s: 1, n: -1}`). The sort is now counted over what the plan handed it, in that order, as a top-k sort with a limit already was, and the profile entry reports mongod's size.

#### Fixed

- `secantus-commands` aggregate / `secantus-core` aggregate: a leading blocking `$sort` without a limit (or with one over 1) is counted over the plan's sort input (`LeadingSort::with_input`), not over the read's sorted output.
- Fixes RM-395 (tests/backlog/test_rm_found_2026_10_26.py::test_leading_sort_spill_storage_size)
- Filed RM-526: a sort by a key many documents share stores its spilled runs in another order (tests/backlog/test_rm_found_2026_11_10_rm395.py::test_tied_sort_spill_storage_size).
