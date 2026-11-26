### `$queryStats` records a `$natural` sort as a hint

A `find` sorted on `$natural` alone is now recorded as mongod 8.2.11 records it.

#### Fixed

- `$queryStats` keyed `{find: "c", sort: {$natural: -1}}` with the sort in its
  query shape and no hint; mongod moves the sort into the key's
  `hint: {$natural: -1}`, so the `keyHash` differed. The key now matches.
  Fixes RM-643 (tests/backlog/test_rm_found_2026_11_14_rm532.py::test_a_natural_sort_is_recorded_as_a_hint)
