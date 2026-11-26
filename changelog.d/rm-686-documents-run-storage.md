### A `$documents` run counts its documents as the storage they were built in

A run of `$documents` now executes the stages mongod desugars it into, so
`metrics.query.sort.totalBytesSorted` and the memory budget count each
document as the storage its expression object built (490 for
`[{a: 1, b: 3}, {a: 2}]` sorted, as mongod 8.2.11), not as read BSON (255).

#### Fixed

- Fixes RM-686 (tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_documents_sort_counts_the_documents_built)
