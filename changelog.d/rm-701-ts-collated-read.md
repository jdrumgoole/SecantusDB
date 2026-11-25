### A time-series read under a collation streams its buckets

A time-series `aggregate` under a collation now reads its buckets through
the cursor stage a buffer at a time, each batch matched under the
collation, as mongod 8.2.11 does, instead of reading the whole collection
when the command runs. A filter holding a string, or naming the meta
field, still reads whole, since the bucket bounds and the index over the
buckets compare without the collation.

#### Fixed

- Part of RM-701: a collated time-series read (RM-701 stays open for a collection with an index of its own).
  Fixes RM-701 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_under_a_collation_buffers_its_buckets)
