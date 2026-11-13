### Rust MongoDB server: only the query layer's `$sort` sets the sort key

A document's sort key now comes from the `$sort` that mongod's query layer
runs, as on mongod 8.2. A `$sort` that the pipeline runs itself, behind an
`$addFields`, a `$project`, a `$limit` or a `$geoNear`, now sorts the documents
and leaves their key as it was. Before, every `$sort` set the key. A later
`{$meta: "sortKey"}` or `$_internalSetWindowFields` `$rank` then read the last
`$sort`'s fields where mongod reads the earlier sort's key, `[]`, or the
`$geoNear` distance.

#### Fixed

- Behind a leading `$sort` and an `$addFields`, the key after a second `$sort`
  stays the first sort's.
- A `$sort` behind a leading `$addFields` or `$project` leaves the empty key.
  So does a `$sort` behind a leading `$limit`.
- A `$sort` behind a `$geoNear` keeps the distance as the key.
- A `$sort` behind an `$unwind` still sets the key when mongod moves it ahead
  of the `$unwind` into the query layer, that is, when it sorts on none of the
  paths the `$unwind` writes.
- A `$lookup` pipeline follows the same rule over its `from` collection, with
  a view's stages in front when `from` is a view.

Fixes RM-523 (tests/backlog/test_rm_found_2026_11_10_rm410.py::test_which_sort_sets_the_key; tests/backlog/test_rm_found_2026_11_10_rm410.py::test_a_sort_the_pipeline_runs_keeps_the_key_it_was_handed)
