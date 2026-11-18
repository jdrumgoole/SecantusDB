### Rust MongoDB server: a `$unionWith` pipeline's `$sort` sets the sort key

A `$sort` inside a `$unionWith` sub-pipeline now sets the sort key that
`{$meta: "sortKey"}` and `$_internalSetWindowFields` ranks read there, as
mongod 8.2.11 does and as a `$lookup` pipeline already did.

#### Fixed

- The sub-pipeline's ranks tied every document and its `$meta` was refused
  with code 2.
  Fixes RM-624 (tests/backlog/test_rm_found_2026_11_13_rm521.py::test_a_union_with_sub_pipeline_sort_sets_the_key)
