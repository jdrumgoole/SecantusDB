### A time-series `$group` on the meta field counts what it examined

The Rust MongoDB server's slow-query line and profile entry for an
`aggregate` over a time-series collection that groups on the meta field and
only counts (`{$sum: 1}`, `{$count: {}}`) now count the buckets its cursor
stage examined, as mongod 8.2.11 does when it runs that group over the
buckets in the classic engine.

#### Fixed

- Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_grouped_time_series_read_counts_what_it_examined)
