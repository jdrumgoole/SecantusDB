### A time-series `$or` over several indexes, and a hinted read under a collation, scan the buckets' indexes

A time-series `aggregate` whose `$match` is an `$or` that an index of its own
answers for every branch now reads the buckets as mongod 8.2.11 does: the
`OR` of the branches' bounded index scans, one after the other, each bucket
once, under a SUBPLAN, instead of reading the whole collection when the
command runs. A hint to a collated index under a collation, and a hint to the
default index under one with a meta filter, scan that index too.

#### Fixed

- Part of RM-701: an `$or` across two indexes of its own and a hinted read under a collation (RM-701 stays open for the trials between plans, a string filter under a collation, and the other indexes it names).
  Fixes RM-701 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_hinted_time_series_read_under_a_collation_scans_the_collated_index, tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_or_read_across_two_indexes_of_its_own_scans_them_as_mongod)
