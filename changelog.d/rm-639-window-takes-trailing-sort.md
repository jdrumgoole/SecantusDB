### A `$sort` right behind a window is taken into the window's own

The Rust MongoDB server now drops a `$sort` that stands right behind a
`$setWindowFields` when its key and the window's sort key share a
prefix, as mongod 8.2.11 does: the window's `$sort` takes the longer
key, and `explain` ends at the `$_internalSetWindowFields`.

#### Fixed

- `[{$set}, {$setWindowFields: {sortBy: {n: 1}}}, {$sort: {n: 1, _id:
  1}}]` lists one `$sort`, `{n: 1, _id: 1}`, ahead of the window, where it
  listed a second `$sort` behind it. As on mongod, a `$skip: 0` between the
  two does not stop this, and any other stage does. So does a window whose
  `$sort` was moved ahead of an `$unwind` first, or one partitioned by a
  computed key. The window's own `sortBy` is unchanged. A second window's
  `$sort` is taken in the same way, so `[$set, W, W, $sort]` keeps its last
  `$sort`.

Fixes RM-639 (tests/backlog/test_rm_found_rm519.py::test_a_sort_after_a_window_on_its_key_is_dropped)
