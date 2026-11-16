### Rust MongoDB server: a `find` of a time-series collection's buckets says what it examined

A clustered collection's read -- a time-series collection's
`system.buckets` among them -- is not streamed, so its `Slow query` line
and profile entry left out what the read examined. A read the first batch
ends now counts its plan's run as `explain`'s `executionStats` does.

#### Fixed

- The line for a `find` of `<db>.system.buckets.ts`, or of any clustered
  collection, that the first batch ends has `keysExamined`, `docsExamined`
  and `nBatches`, as mongod 8.2.11's has. A read that leaves a cursor open
  is still uncounted (RM-708).

Fixes RM-586 (tests/backlog/test_rm_ts_ddl_lines.py::test_a_find_of_buckets_says_what_it_examined)
