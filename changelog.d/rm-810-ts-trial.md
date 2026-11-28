### A time-series read is tried between its plans as mongod tries them

A read of a time-series collection that more than one plan can answer now
runs mongod's trial over its buckets, as a read of any collection does: the
default index's scan beside the clustered scan of the bucket `_id`s that the
time field bounds, the scan of each index of its own, and an `$or` as the
`OR` of its branches (each branch with a trial of its own where two indexes
answer it). The plans are raced and ranked as mongod ranks them and the plan
cache is consulted and written. The winner's results are the cursor stage's
whole first buffer (up to 101 buckets), and the buffers go on to 224 buckets
in all, as the buffers of 32, 64 and 128 do when only one plan can answer.

#### Fixed

- A meta and time filter with the default index only reads as mongod's trial
  between the index scan and the clustered scan does. Fixes RM-810
  (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_meta_and_time_read_is_tried_against_the_clustered_scan).
- A meta filter beside an `$or` of two indexes of its own, and an `$or`
  branch two indexes answer, read as mongod reads them. Part of RM-701
  (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_or_read_beside_a_meta_filter_is_tried_against_the_default_index,
  ::test_a_time_series_or_read_with_a_branch_of_two_indexes_matches_mongod).
