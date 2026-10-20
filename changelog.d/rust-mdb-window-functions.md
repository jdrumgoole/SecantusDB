### Rust MongoDB server: covariance, `$topN` windows, and mongod's window arithmetic

The window functions that were missing or drifted now answer as mongod
8.2.11 does. Each was transcribed from mongod's own source (r8.2.1) and
checked against a live server.

#### Fixed

- `$covariancePop` and `$covarianceSamp` work over any window. They use
  mongod's online algorithm: two windowed means and a removable co-moment
  sum. Pairs that are not two numbers are ignored, and NaN and the
  infinities are handled as mongod handles them.
- `$topN` and `$bottomN` are accepted as window functions; they were 168.
- `$linearFill` interpolates over a date sortBy, in mongod's value
  arithmetic. Its refusals now carry mongod's codes: a non-numeric value, a
  repeated sort value, and mixed sort types.
- `$derivative` and `$integral` with a `unit` no longer drift from about
  the 11th significant digit. The quotient is taken in milliseconds and
  scaled afterwards, and `$integral` keeps mongod's removable sum of
  trapezoids. Decimal inputs stay decimal. Their errors are mongod's: 9 for
  a `$derivative` without bounds, and 5624900–5624903 and 5423900–5423902.
- Decimal division of a value whose 35th digit is an exact tie, such as
  `-222.2918400000001378633333333333333 / 2`, answered "can't $divide by
  zero". It now rounds half to even, as long division's exact digits allow.

Fixes RM-214 (`tests/backlog/test_rm_found_2026_10_20b.py::test_window_functions`).
