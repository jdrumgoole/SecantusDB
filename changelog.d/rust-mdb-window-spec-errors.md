### Window specs the Rust MongoDB server refused by shrug, now refused by name

A `$setWindowFields` spec that mongod rejects used to come back from the Rust
MongoDB server as code 2, "aggregation pipeline uses a stage or operator not
supported by the Rust server" — the same answer whether the spec was malformed
or merely unimplemented, which told a client nothing about which. Each of
those refusals now carries mongod's own code and message, measured against
mongod 8.2.1: 605001 for a `$linearFill` without a one-element `sortBy`,
5339902 and 8947401 for range bounds over a multi-field or descending `sortBy`,
5429414 from the executor for a range window whose sort value is not a number,
14 for a `sortBy` of the wrong type, and 9 for a `$shift` with a `window`, a
`$shift` without a `sortBy`, a `window` on `$locf` or `$linearFill`, two
functions in one output field, and a name that is not a window function at all.

Two shapes went the other way: mongod *accepts* an empty `output` and a `$locf`
with no `sortBy`, and the Rust server was refusing both. An empty `output` now
passes the partition's documents through, and `$locf` carries its last
observation forward in the partition's own order.

`$expMovingAvg` is fixed in the same pass. It used to refuse a partition
holding a null, a missing field, a string or a Decimal128 outright. mongod
skips an input that is not a number — the average does not move, and the row
answers the average as it stands — and it carries that average in Decimal128
throughout, printing a double until a Decimal128 input arrives. Reproducing
that model also corrected the plain double answers: mongod's `{N: 2}` over
`[10, 0.1]` is `3.4`, where the same steps in IEEE doubles drift to
`3.400000000000001`. A 1,170-case sweep against mongod 8.2.1 went from 14
divergences to none.

#### Fixed

- `$setWindowFields` refusals carry mongod's code and message rather than a
  generic deferral. Fixes RM-226
  (tests/backlog/test_rm_found_2026_10_21.py::test_window_spec_errors_carry_mongods_codes,
  widened to every shape measured on 2026-11-06).
- An empty `output` is a pass-through, as on mongod, instead of a refusal
  (RM-367,
  tests/backlog/test_rm_found_2026_10_21.py::test_an_empty_output_passes_the_documents_through).
- `$locf` no longer requires a `sortBy` (RM-368,
  tests/backlog/test_rm_found_2026_10_21.py::test_locf_needs_no_sort_by).
- `$expMovingAvg` averages past a non-numeric input and makes the average
  decimal at mongod's scale, and its double answers come from the decimal
  average rather than IEEE steps. Fixes RM-225
  (tests/backlog/test_rm_found_2026_10_21.py::test_exp_moving_avg_over_null_missing_and_decimal,
  ::test_exp_moving_avg_with_an_alpha_over_a_decimal,
  ::test_exp_moving_avg_doubles_come_from_the_decimal_average).
