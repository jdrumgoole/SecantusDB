### A `createIndexes` that builds nothing names `<db>.$cmd` on its line

The Rust MongoDB server's `Slow query` line for a `createIndexes` whose
indexes all exist already now names `<db>.$cmd`, `normal`, as mongod 8.2.11's
does, for a time-series collection as for any other.

#### Fixed

- A no-op `createIndexes` line named the collection (or a time-series
  collection's buckets) instead of `<db>.$cmd`.

Fixes RM-582 (tests/backlog/test_rm_ts_ddl_lines.py::test_a_create_indexes_that_builds_nothing_names_cmd)
