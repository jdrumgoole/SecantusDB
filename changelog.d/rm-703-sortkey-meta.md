### `find` returns a projection's `{$meta: "sortKey"}` field

A `find` with a sort and a projection field `{$meta: "sortKey"}` now gives
each document its sort key as an array, after the projected fields, as
mongod 8.2.11 does -- in the first batch and in every getMore.

#### Fixed

- The Rust MongoDB server dropped a top-level `{$meta: "sortKey"}`
  projection field outside `returnKey`.
  Fixes RM-703 (tests/backlog/test_rm_found_rm568.py::test_find_keeps_a_sort_key_meta_field)
