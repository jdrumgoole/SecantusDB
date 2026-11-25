### A hinted time-series read scans the index over the buckets

A time-series `aggregate` hinted to the default meta-and-time index, by
name or by key pattern, now reads that index over the buckets a buffer at
a time, as mongod 8.2.11 does, instead of reading the whole collection when
the command runs. A `{$natural: 1}` hint reads as no hint does.

#### Fixed

- Part of RM-701: a hint to the default time-series index (RM-701 stays open for collations and other indexes).
  Fixes RM-701 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_hinted_time_series_read_scans_the_index_over_the_buckets)
