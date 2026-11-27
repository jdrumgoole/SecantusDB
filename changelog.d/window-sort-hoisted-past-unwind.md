### `$setWindowFields` behind an `$unwind` orders ties as mongod does

The Rust MongoDB server now sorts the documents an `$unwind` takes when the
`$setWindowFields` behind it has a `$sort` mongod hoists ahead of that
`$unwind`, so tied documents come back in mongod's `std::sort` order of the
source documents, each followed by its other unwound copies.

#### Fixed

- `[{$unwind: "$a"}, {$setWindowFields: {sortBy: {k: 1}, ...}}]` returned
  ties in the order read; mongod 8.2.11 returns them sorted at the
  `$unwind`.

Fixes RM-640 (tests/backlog/test_rm_found_rm527.py::test_window_sort_hoisted_past_unwind_orders_the_source_documents)
