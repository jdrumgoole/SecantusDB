### Rust MongoDB server: a time-series index build logs its build thread's line

An index build of a time-series collection that holds measurements now
writes the build thread's `Slow query` line beside the command's, naming
`<db>.system.buckets.<coll>`, as mongod 8.2.11 does.

#### Fixed

- The build asked the time-series collection's own (always empty) document
  table whether it scans anything, rather than its buckets, and so took the
  build for one of an empty collection.

Fixes RM-583 (tests/backlog/test_rm_ts_ddl_lines.py::test_a_time_series_index_build_has_its_line)
