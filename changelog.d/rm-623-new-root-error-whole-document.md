### A `$replaceRoot` error names the whole document when the sort key is read later

When a stage after a `$replaceRoot` or `$replaceWith` reads
`{$meta: "sortKey"}`, mongod 8.2.11 does not prune the documents the stage
reads, so its 40228 error names the whole input document. The Rust MongoDB
server now does the same, rather than naming only the fields the `newRoot`
reads.

#### Fixed

- Fixes RM-623 (tests/backlog/test_rm_found_2026_11_13_rm520.py::test_a_new_root_error_behind_a_sort_names_the_document)
