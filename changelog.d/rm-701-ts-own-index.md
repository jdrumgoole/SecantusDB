### A time-series read beside an index of its own streams its buckets

A time-series collection with an index of its own no longer makes every
read load the whole collection at the command: a filter that index cannot
serve reads its buckets through the cursor stage, as mongod 8.2.11 does.

#### Fixed

- An `aggregate` or `find` over a time-series collection with a secondary
  index returns what mongod's cursor had buffered after a concurrent
  delete (2,303, not 3,999), when the filter names none of that index's
  fields.

Fixes RM-701 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_beside_an_index_of_its_own_buffers_its_buckets), part
