### An aggregate's sort of one counts every key it sorted

An aggregate whose leading `$sort` is followed by `$limit: 1` now adds
every document its sort read to `metrics.query.sort.totalKeysSorted`, as
mongod 8.2 does: 6 for `[{$match: {z: 0}}, {$sort: {y: 1}}, {$limit: 1}]`
over six matching documents, and 300 for an SBE sort of 300 documents in
front of a `$group`. The Rust MongoDB server counted only the one document
the sort kept. Limits of two and more already counted every
document the sort read.

#### Fixed

- The aggregate command keeps what its read's blocking sort took in for a
  limit of one too, and the pipeline's leading `$sort` counts its keys over
  that input, as mongod's `LimitOneSorter` sees each one (it still counts
  no bytes, RM-374). Regression test:
  `aggregate::tests::a_leading_sort_of_one_counts_every_key_its_read_sorted`.
- Fixes RM-468 (tests/backlog/test_rm_found_rm374.py::test_an_aggregate_sort_with_a_limit_counts_every_key_it_sorted)
- Fixes RM-505 (tests/backlog/test_rm_326_sort_counts.py::test_sbe_limited_sort_counts_every_key)
