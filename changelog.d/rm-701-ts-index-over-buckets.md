### A time-series index of its own is kept over the buckets, and reads plan over it

An ascending or descending index on a time-series collection is now built
over its buckets as mongod 8.2.11 builds it (`{control.min.v: 1,
control.max.v: 1}`), and a read plans over it with the server's planner
instead of reading the whole collection at the command.

#### Fixed

- A filter on an indexed measurement field (`v < 100`, `v = 5`) scans the
  index over the buckets, and one that bounds only the second key field
  (`v >= n`) reads the buckets in order, as mongod's planner does; a hint
  to an index of the collection scans it. A cursor over such a read
  returns what mongod's had buffered after a concurrent delete (2,303, 63
  or 1,279, not 3,999).
- `dropIndexes` takes the index's copy over the buckets away with the
  collection's, `m_1_t_1` included: a meta filter after dropping it reads
  the buckets in order.
- The `planSummary` of an `aggregate` with a `hint` names the hinted index
  (it ignored the hint and said `COLLSCAN`).
- `collStats` sizes an index of its own over the buckets, where it
  reported 0.

Fixes RM-701 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_selective_filter_scans_the_index_over_the_buckets, tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_of_its_own_index_buffers_as_mongod, tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_hinted_read_of_no_filter_names_the_index_over_the_buckets), part
