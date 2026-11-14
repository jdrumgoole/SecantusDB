### Rust MongoDB server: an aggregate `$sort` on a `$meta` key runs

A `$sort` stage keyed by `{$meta: "randVal"}` or `{$meta: "searchScore"}`
was refused as an unsupported stage; it now sorts as mongod 8.2 does.

#### Fixed

- `{$sort: {s: {$meta: "randVal"}}}` orders the documents by a random value
  drawn for each, and a `searchScore` key, which has no value without
  `$search`, ties every document, so the other keys decide the order.
  Fixes RM-423 (tests/backlog/test_rm_found_2026_11_08_rm332.py::test_a_sort_on_a_meta_key_runs)
