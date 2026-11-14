### Rust MongoDB server: a compound index scan over several intervals examines mongod's keys

A scan of `a_1_c_1` for `{a: {$in: [3, 60]}, c: {$in: [1, 5]}}` now
examines 16 keys with 5 seeks, as mongod 8.2.11's does, in `explain` and in
the plan-cache trial whose works score the candidates.

#### Fixed

- Past the last interval of a later field, the scan ends when every earlier
  field is on the point that is its last interval, as mongod's
  `IndexBoundsChecker` does, instead of examining one more key.
- A seek to a field's next interval lands at the start of the later fields'
  first intervals, so the keys before them are not examined. A losing
  `$nin` candidate over three intervals of one field now seeks and returns
  as mongod's does in the trial, and scores the same.

Fixes RM-433 (tests/backlog/test_rm_found_2026_11_08_rm342.py::test_a_scan_of_two_in_lists_examines_mongods_keys)
Fixes RM-438 (tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_nin_candidate_reads_mongods_keys)
