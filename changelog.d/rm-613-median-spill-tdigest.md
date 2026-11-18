### A spilled `$median` / `$percentile` is mongod's t-digest

A classic `$group` over its memory budget now writes each `$median` /
`$percentile` to its spill file as mongod 8.2.11's `TDigest::serialize`
does -- `[negInfCount, posInfCount, min, max]` then each centroid's weight
and mean -- so the spill file is mongod's size (5561 bytes for 300 groups
of one value, where it was 2948).

#### Fixed

- The spilled state of an approximate `$median` / `$percentile`, down to
  mongod's starting `min` / `max` (`numeric_limits<double>::max()` and
  `::min()`).
  Fixes RM-613 (tests/backlog/test_rm_found_2026_11_13_rm325.py::test_a_median_spills_mongods_state)

Filed RM-720: an approximate `$median` ranks the exact values where mongod
interpolates its t-digest.
