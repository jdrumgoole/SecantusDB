### A time-series `$group` on the meta field that sums `1.0` answers an int

mongod 8.2.11 rewrites a `$group` on a time-series collection's meta field
whose accumulators only count to a sum of each bucket's `control.count`, so
`{$sum: 1.0}` answers an int. The Rust MongoDB server summed `1.0` per
measurement and answered a double; it now makes the same rewrite.

#### Fixed

- `{$group: {_id: "$meta", n: {$sum: 1.0}}}` over a time-series collection
  answers `n` as an int, as mongod does.

Fixes RM-719 (tests/backlog/test_rm_355_364.py::test_a_meta_group_summing_one_point_zero_answers_an_int)
