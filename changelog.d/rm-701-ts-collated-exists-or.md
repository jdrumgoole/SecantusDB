### A time-series read beside a collated or `$exists` index, or an `$or`, plans as mongod does

A collated index, and a partial index of `$exists: true`, on a time-series
collection are now kept over its buckets as mongod keeps them, and an `$or`
read is planned the way mongod plans one: under a SUBPLAN.

#### Fixed

- A read beside a collated index (`v < 100` under that collation) or a
  partial index of `$exists: true` goes through the planner of the buckets
  as mongod's does, so a cursor over it returns what mongod's had buffered
  after a concurrent delete (63, not 99).
- An `$or` beside an index of its own is one scan of the buckets when a
  branch has no bounds, and the one index scan when each has, instead of a
  read of the whole collection (63, not 198).

Fixes RM-701 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_beside_a_collated_index_scans_it_as_mongod, tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_beside_an_exists_partial_index_scans_it_as_mongod, tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_or_read_over_an_index_of_its_own_scans_it_as_mongod), part
