### Rust MongoDB server: `returnKey` keeps a projection's sort key

A `find` with `returnKey` now keeps a projection's `{$meta: "sortKey"}`
fields, as mongod 8.2 does, and a `sortKey` projection with no sort is
refused.

#### Fixed

- `find({z: 0}, {k: {$meta: "sortKey"}}).sort({z: 1}).returnKey()`
  returns `{k: [0]}` for each document, after the index key when the read
  scans one, where it returned the sort's own field (`{z: 0}`) or the
  index key alone.
- A `{$meta: "sortKey"}` projection in a read with no sort is refused with
  mongod's `BadValue` ("cannot use sortKey $meta projection without a
  sort"), `returnKey` or not; it used to return the documents.

Fixes RM-568 (tests/backlog/test_rm_found_rm470.py::test_return_key_keeps_a_sort_key_meta_field, tests/backlog/test_rm_found_rm568.py::test_return_key_keeps_sort_keys_beside_an_index_key, tests/backlog/test_rm_found_rm568.py::test_a_sort_key_meta_field_without_a_sort_is_refused). Files RM-703.
