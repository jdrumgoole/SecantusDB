### Rust MongoDB server: a SORT_MERGE candidate's trial counts match mongod's

A plan cache entry whose candidate merges two sorted index scans now
reports the trial's counts as mongod 8.2.11 does.

#### Fixed

- The FETCH above a SORT_MERGE no longer works the merge once more after
  the merge has handed over its last document (`FetchStage::doWork`
  answers end-of-stream itself), so the SORT_MERGE's `works` is mongod's,
  and the SORT_MERGE reports `dupsTested` and `dupsDropped`, counted when
  a child reads a multikey index.

Fixes RM-437 (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_sort_merge_candidate_counts_as_mongods)
