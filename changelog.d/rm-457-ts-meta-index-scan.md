### A time-series read of the meta field scans the index over the buckets

The Rust MongoDB server now builds a time-series collection's default
`m_1_t_1` index over its buckets, keyed as mongod keys it there
(`{meta: 1, control.min.t: 1, control.max.t: 1}`), and an `aggregate`
filtering on the meta field reads the buckets in that index's order, 32 at
a time and series by series, as mongod 8.2.11 does.

#### Fixed

- A time-series `aggregate` filtering on the meta field read the whole
  collection at the command instead of buffering buckets from the index.
  Fixes RM-457 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_of_the_meta_index_buffers_its_buckets)
