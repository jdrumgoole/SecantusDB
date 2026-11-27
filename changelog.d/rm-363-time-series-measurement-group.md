### A time-series `$group` after a measurement `$match` counts what it examined

The slow-query line of an `aggregate` that groups what a `$match` on a
measurement field read from a time-series collection now counts the buckets
examined, as mongod 8.2.11's does (five of five), where it counted nothing.

#### Fixed

- The line's `docsExamined`, `keysExamined` and `cursorExhausted` for a
  time-series `$match` on a measurement field followed by a `$group`.
  Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_group_after_a_measurement_match_counts_what_it_examined)
