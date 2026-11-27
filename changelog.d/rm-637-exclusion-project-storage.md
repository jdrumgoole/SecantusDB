### A `$sort` behind an exclusion `$project` counts what mongod does

The Rust MongoDB server now models the storage an exclusion `$project`
(and `$unset`) leaves each document with, as mongod 8.2's
`ExclusionNode` builds it, so a `$sort` after one reports the same
`totalDataSizeSortedBytesEstimate` and spills at the same point.

#### Fixed

- An exclusion `$project` behind an `$unwind` was counted as the BSON each
  document encodes (2350 bytes over ten documents for `{b: 0}`, where
  mongod 8.2.11 counts 4640). A copy of the input's storage now carries
  each removed field, a dotted path projects its sub-document into a copy
  of that sub-document's storage, the top-level removals come before
  those sub-documents, and a removed field the input lacks leaves the
  cache alone. `$unset` of a dotted path takes the same model (5950 bytes
  where mongod counts 6930).
- `{$project: {_id: 0}}` alone was read as an inclusion of no fields when
  deciding what the scan keeps; it is an exclusion.

Fixes RM-637 (tests/backlog/test_rm_found_rm519.py::test_a_sort_counts_what_an_exclusion_project_left)
