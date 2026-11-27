### A `$meta`-keyed `$sort` before a narrowing stage is refused as mongod refuses it

An aggregate that sorts on a `{$meta: "randVal"}` or `{$meta: "searchScore"}`
key and then reads only some fields now answers mongod 8.2.11's 16410.

#### Fixed

- `[{$sort: {a: 1, s: {$meta: "randVal"}}}, {$project: {a: 1}}]`, and the
  same sort before a `$group` on a field, an `$unset`, an `$unwind` or a
  `$replaceRoot`, is refused with 16410 naming the `$computed<n>` field
  mongod's query layer adds to its pushed-down projection. A `$limit`
  taken into the sort, a projection that reads metadata, stages that need
  the whole document or none of its fields, and a missing collection
  answer as before, as on mongod.

Fixes RM-691 (tests/backlog/test_rm_found_2026_11_08_rm332.py::test_a_rand_sort_beside_a_field_under_a_project_is_refused)
