### `$bucketAuto` `granularity` validation

The optional `granularity` argument to `$bucketAuto` is now validated against
real mongod's rules instead of being silently ignored. A non-string value
raises code 40261, and an unknown series name raises 40257 — both matching
mongod 7.0.12 exactly. A *valid* preferred-number series (`R5`, `R10`, …,
`POWERSOF2`, `1-2-5`, `E6`, …) is rejected as not-yet-supported (code 2) rather
than silently producing count-chunked, unrounded boundaries: reproducing
mongod's boundary rounding byte-for-byte would require its exact internal
float series constants (its `6.3` is the f64 `6.3000000000000007`, not
`float("6.3")`), which aren't recoverable by black-box probing — so a faithful
error is preferred over a silently-divergent result (see `tasks/backlog.md`).
Both the Python and Rust servers behave identically.

#### Fixed

- `$bucketAuto` with a non-string `granularity` now raises `Location40261`, and
  with an unknown series name `Location40257`, instead of accepting them.

#### Changed

- `$bucketAuto` with a valid but unsupported `granularity` series now raises an
  explicit "not yet supported" error (code 2) instead of silently ignoring the
  field and returning unrounded boundaries.
