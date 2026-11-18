### A sorted time-series read counts what it examined

A `find` or `aggregate` of a time-series collection with a `$sort` now
reads its buckets through the cursor stage and sorts once over them, so
its slow-query line and profile entry count the buckets examined as
mongod 8.2.11's do, where before it read the collection whole and
counted nothing.

#### Fixed

- Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_time_series_read_made_whole_counts_what_it_examined)
