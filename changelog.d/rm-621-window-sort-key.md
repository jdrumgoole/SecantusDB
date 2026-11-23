### A window function reads each document's sort key

`{$meta: "sortKey"}` inside a `$setWindowFields` or
`$_internalSetWindowFields` output now gives each document's sort key, as
mongod 8.2.11 does: the key of the `$sort` in front, or for a sorting
`$setWindowFields` the key of the sort it runs behind (partition, then
`sortBy`).

#### Fixed

- `$setWindowFields` refused `{$meta: "sortKey"}` in an output (code 2),
  and `$_internalSetWindowFields` read no key there.

Fixes RM-621 (tests/backlog/test_rm_found_2026_11_13_rm520.py::test_a_windows_output_reads_the_sort_key)
