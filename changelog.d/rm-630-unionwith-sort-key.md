### A `$unionWith` passes the sort key on

The Rust MongoDB server now keeps a document's sort key through a
`$unionWith`, as mongod 8.2 does: the outer documents keep the key of the
`$sort` in front, and the documents the stage adds carry their
sub-pipeline's (the empty key with no `$sort`).

#### Fixed

- `{$meta: "sortKey"}` behind a `$unionWith` read `[]` for every document,
  and a rank behind one tied every outer document.

Fixes RM-630 (tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_union_passes_the_sort_key_on)
