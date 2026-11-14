### Rust MongoDB server: a `$lookup` on an empty `localField` array joins what null joins

A `$lookup` whose `localField` holds an empty array now joins the foreign
documents whose `foreignField` is missing, null or `[null]`, as mongod 8.2.11
does. It joined none.

#### Fixed

- An empty `localField` array is matched as null in every `$lookup` join
  path: the simple form, the simple form over an index on the foreign field,
  the pipeline form with `localField` / `foreignField`, and a `$lookup`
  without `from`. Fixes RM-529
  (tests/backlog/test_rm_found_2026_11_10_lookup.py::test_an_empty_local_array_joins_what_null_joins).
