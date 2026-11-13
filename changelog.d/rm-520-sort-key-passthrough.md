### Rust MongoDB server: the sort key passes through `$replaceRoot`, `$lookup` and `$setWindowFields`

A document's sort key now reaches the stages after a `$replaceRoot` or
`$replaceWith`, a `$lookup` and a `$setWindowFields`, as on mongod 8.2. Before,
the key stopped at each of these stages. A later `{$meta: "sortKey"}` read
`[]`, and a `$_internalSetWindowFields` `$rank` gave every document 1. The
`$geoNear` distance and point now pass through the same stages too, where a
later `$meta` for either was refused with 40218.

#### Fixed

- `$replaceRoot` and `$replaceWith` keep the sort key and the `$geoNear`
  metadata on each new document, and a `{$meta: "sortKey"}` inside `newRoot`
  reads the key instead of being refused.
- A `$lookup` keeps them, in either engine. A `localField` `$lookup` that runs
  in the slot-based engine used to drop them at the engine change.
- A `$setWindowFields` with no `partitionBy` or `sortBy` passes on the key it
  was given. One that sorts sets the key of the `$sort` it runs behind: the
  partition, then `sortBy`. A computed partition is evaluated for the key and
  a constant one is left out. While the stage runs, the key is held off the
  documents, so `$$ROOT` in a window does not show it.

Fixes RM-520 (tests/backlog/test_rm_found_2026_11_10_rm410.py::test_the_sort_key_passes_through_stages_that_keep_it)
