### An approximate `$median` / `$percentile` answers from mongod's t-digest

A `$group`'s `$median` and `$percentile` now compute mongod 8.2's t-digest
(`k2` scaling, delta 2000, a flush every 6000 values, interpolation between
centroid means) instead of the exact discrete rank, so the answer matches
`mongod`'s to the last digit.

#### Fixed

- Over 5999 values of `(i * 7919) % 30011 * 0.37` the median is now
  5552.219999999999, and over 20000 5551.609969919785, as mongod 8.2.11
  answers (it was 5552.22 and 5551.48). The expression form and
  `$setWindowFields` keep their existing algorithms.

Fixes RM-720 (tests/backlog/test_rm_found_2026_11_18_rm613.py::test_an_approximate_median_is_mongods_tdigest)
