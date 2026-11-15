### Rust MongoDB server: `explain` explains a `$unionWith`'s sub-pipeline

`explain` of an aggregate with a `$unionWith` over a collection now lists
that sub-pipeline as mongod 8.2.11 does: a `$cursor` of its own, with the
sub-query's `queryPlanner` (and `executionStats`, `nReturned` and yields at
the higher verbosities), then the stages the cursor did not absorb. The
server listed the stages as written.

#### Fixed

- Fixes RM-426 (tests/backlog/test_rm_found_2026_11_08_explain_listing.py::test_union_with_sub_pipeline_is_explained)
- Fixes RM-681 (tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_union_with_over_a_collection_lists_its_cursor)

Filed RM-696 (a fusion input pipeline's `$unionWith` still lists no
`$cursor`) and RM-697 (a `DISTINCT_SCAN` `$group`'s query hash).
