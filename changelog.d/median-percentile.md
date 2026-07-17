### $median and $percentile land on both servers — no t-digest required

The `$median` and `$percentile` group accumulators — and their expression
forms over arrays — now run on both the Python and Rust servers, with
semantics pinned by a live probe against real mongod 7.0.12. On bounded data
mongod's "approximate" method resolves to a discrete percentile —
`sorted[max(0, ceil(p·n) − 1)]`, returned as a double — so no approximate
t-digest sketch is needed and the two engines agree exactly: values collect
from int, long, double, and Decimal128 inputs (as doubles), bool and NaN are
excluded, and an empty input yields null (median) or per-`p` nulls
(percentile), all exactly as mongod behaves.

Spec validation carries mongod's verbatim codes and messages: a missing
`method` / `input` / `p` field is `40414`, a method other than
`"approximate"` is rejected with mongod's exact wording, a non-array `p` is
`7750301`, and an out-of-range `p` value is `7750303`.

#### Added

- `$median` / `$percentile` as `$group` accumulators and as expression
  operators, on both servers, with curated parity coverage, unit tests, and
  wire tests against each server.
