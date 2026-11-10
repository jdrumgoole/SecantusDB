### A `distinct` of a view says how long it planned

mongod runs a `distinct` of a view as an aggregation over the collection
beneath the view, and plans that read as it plans any other: its `Slow
query` line carries the `planningTimeMicros`, as a `distinct` of a
collection's does. The Rust MongoDB server now plans the same read --
the view's pipeline, then the query, over the collection beneath -- so the
line says how long the planning took, whether the plan reads an index,
scans the collection, or is the `EOF` of a view on a collection that is
not there.

#### Fixed

- A `distinct` of a view plans the read beneath it whether or not
  `notablescan` is set (`notablescan::plan_and_check`), and its `Slow
  query` line carries `planningTimeMicros`, as mongod 8.2.11's does.
  Fixes RM-381 (tests/backlog/test_rm_view_lines.py::test_a_distinct_of_a_view_says_how_long_it_planned),
  with regression tests for a view that scans and a view of no collection
  (`test_a_distinct_of_a_view_that_scans_says_how_long_it_planned`,
  `test_a_distinct_of_a_view_of_no_collection_says_how_long_it_planned`).
- Filed while working it: RM-493 (a `distinct` of a view counts nothing
  it examined), RM-494 (a `count` or `aggregate` of a view names a
  `COLLSCAN` where mongod's plan reads an index).
