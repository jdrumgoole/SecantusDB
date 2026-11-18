### A time-series `$count` stage counts what it examined

The Rust MongoDB server's slow-query and profiler line for `{$count: "n"}`
over a time-series collection now counts the buckets it examined, as
mongod 8.2.11 does. mongod runs the stage over the buckets as a `$group`
on a constant and a `$project`.

#### Fixed

- Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_count_stage_time_series_read_counts_what_it_examined)
