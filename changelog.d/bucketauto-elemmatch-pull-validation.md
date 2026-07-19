### Argument validation for `$bucketAuto`, projection `$elemMatch`, and `$pull` / `$pullAll`

Three more type-guard divergences from real mongod are closed. `$bucketAuto`
now validates its `buckets` argument (a bool or non-numeric value, a fractional
double, a non-positive count, or a missing `groupBy`/`buckets` each raise
mongod's exact code, while a whole-double count is accepted); a non-document
`$elemMatch` projection argument is rejected; and `$pull` / `$pullAll` against a
field that is present but not an array now errors instead of silently doing
nothing. All three are covered on both the Python and Rust servers (the Rust
core defers each invalid case) and verified against real mongod 7.0.12.

#### Fixed

- `$bucketAuto` `buckets` now raises `Location40241` (non-numeric or bool),
  `Location40242` (fractional double — not representable as a 32-bit integer),
  `Location40243` (not greater than 0), and `Location40246` (missing `groupBy`
  or `buckets`) instead of silently accepting `buckets: true` or leaking an
  uncoded error. A whole-double `buckets` (e.g. `2.0`) is accepted, matching
  mongod.
- A non-document `$elemMatch` projection argument (e.g. `{arr: {$elemMatch: 5}}`)
  now raises `Location31274` instead of being silently accepted.
- `$pull` / `$pullAll` on a field that exists but is not an array (a scalar or
  `null`) now raises code 2 ("Cannot apply $pull to a non-array value") instead
  of silently doing nothing. A missing field remains a no-op.
