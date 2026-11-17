### A time-series read bounded on its time field names a clustered scan

The Rust MongoDB server's slow-query line and profile entry for a read of a
time-series collection bounded by a Date on its time field now say
`CLUSTERED_IXSCAN`, as mongod 8.2.11's do: its buckets are clustered by
`_id`, whose timestamp is each bucket's start.

#### Fixed

- A `find({t: {$gt: <date>}})` of a time-series collection, with no index
  serving the bound, reported `planSummary: COLLSCAN`; mongod reports
  `CLUSTERED_IXSCAN`. A bound by another type, `$ne`, `$not`, `$nor` or an
  `$or` arm still reads as `COLLSCAN`, as on mongod.

Fixes RM-600 (tests/backlog/test_rm_found_2026_11_10_rm363.py::test_a_time_series_read_bounded_on_its_time_field_scans_the_clustered_buckets)
