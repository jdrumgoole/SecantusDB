### A pushed-down `$project` of the sort key with no `$sort` is refused as mongod refuses it

The Rust MongoDB server called a pipeline unsupported when its first
`$project` behind only `$match`es read `{$meta: "sortKey"}` with no
`$sort`. mongod 8.2.11's query planner refuses that projection before it
reads a document, and now this server does too, with the same code and
message.

#### Fixed

- `[{$project: {t: {$meta: "sortKey"}}}]` (alone, behind `$match`es or a
  `{$skip: 0}`) gives code 2 "Failed to determine whether query system can
  provide a DISTINCT_SCAN grouping :: caused by :: cannot use sortKey $meta
  projection without a sort", on a missing collection too.

Fixes RM-634 (tests/backlog/test_rm_found_2026_11_13_rm524.py::test_a_sort_key_projection_without_a_sort)
