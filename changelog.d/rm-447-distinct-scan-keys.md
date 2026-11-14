### Rust MongoDB server: a DISTINCT_SCAN counts the keys its trailing bounds reject

A DISTINCT_SCAN now does one unit of work per key it examines, as mongod's
`DistinctScan::doWork` does. A key that a seek lands on and the bounds on a
later field reject counts as examined, and `needTime` counts that work. Before
this, `totalKeysExamined` and `works` counted only the keys the scan returned.

#### Fixed

- `distinct("x", {x: {$gt: 0}, y: {$gt: 3}})` over `x_1_y_1` examines six
  keys in seven works, as on mongod 8.2.11, both in `executionStats` and in a
  `$group`'s trial. Before this it examined three keys in four works.
  Fixes RM-447 (tests/backlog/test_rm_found_2026_11_08_rm310.py::test_a_distinct_scan_bounded_on_a_trailing_field_examines_mongods_keys)
