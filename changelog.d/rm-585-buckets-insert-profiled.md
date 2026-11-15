### Rust MongoDB server: an `insert` into a time-series collection's buckets is profiled

An `insert` into `<db>.system.buckets.<name>` wrote its slow-query line but
no profile entry; mongod 8.2.11 profiles it as an insert into any collection.

#### Fixed

- The bucket-namespace insert is marked as having run, so the profiler
  writes its entry.

Fixes RM-585 (tests/backlog/test_rm_ts_ddl_lines.py::test_an_insert_into_buckets_is_profiled)
