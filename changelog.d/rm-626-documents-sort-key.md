### A rank behind `$documents` and a `$sort` is refused, as on mongod

On mongod 8.2.11 the documents `$documents` makes carry no sort key, and a
`$sort` the pipeline runs writes none, so a `$rank`, `$denseRank` or
`$documentNumber` behind them is refused with 9973200. The Rust MongoDB
server tied every document instead, in a collectionless aggregate and in a
`$lookup` pipeline alike.

#### Fixed

- `$documents` now leaves its documents keyless, and a collectionless
  aggregate runs the sort-key rewrite, so the rank is refused with mongod's
  9973200.

Fixes RM-626 (tests/backlog/test_rm_found_2026_11_13_rm521.py::test_a_sort_behind_documents_sets_no_key)
