### A time-series `count_documents` counts what it examined

The Rust MongoDB server now reads a time-series `$group` on a constant that
only counts -- what `count_documents` sends -- through its cursor stage over
the buckets, as mongod 8.2.11 does, so its slow-query line and profile entry
count the buckets it examined.

#### Fixed

- Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_counted_time_series_read_counts_what_it_examined)
