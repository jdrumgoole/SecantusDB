### Rust MongoDB server: a `count_documents` counts index keys

A `count_documents` whose filter one plain index bounds -- of a collection,
or of a view over it -- now runs the count scan mongod 8.2.11 runs, as SBE's
`ixseek` runs it: its log line and profile entry name `COUNT_SCAN { x: 1 }`,
count the keys within the bounds and examine no document. It fetched every
matching document through an `IXSCAN` before.

#### Fixed

- A pipeline whose query one index scan answers and whose one pushed stage
  is a `$group` or `$count` that reads no field reads the index's keys
  alone (`turnIxscanIntoCount`), and its `planSummary` says so.

Fixes RM-494 (tests/backlog/test_rm_view_lines.py::test_a_count_documents_runs_a_count_scan)
