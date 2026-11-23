### The sort key passes through `$graphLookup` and `$fill`

A `{$meta: "sortKey"}` or a rank behind a `$graphLookup` or a `$fill`
now reads the key of the `$sort` in front, as on mongod 8.2.11, where the
Rust MongoDB server ended it at each stage and tied every document.

#### Fixed

- A `$graphLookup` and a `$fill` of values keep the sort key they are
  handed. A `$fill` that fills an output by a `method` sets the key of
  the window mongod runs it as: its `partitionBy` (or
  `partitionByFields`) and then its `sortBy`.

Fixes RM-632 (tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_graph_lookup_and_a_fill_keep_the_sort_key, tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_fill_by_a_method_sets_its_windows_sort_key)
