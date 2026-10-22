### Rust MongoDB server: expressions answer as mongod instead of "not supported"

Wherever the expression engine met an argument it had not been taught, it
answered 2 "aggregation pipeline uses a stage or operator not supported by
the Rust server" -- a relic of the days when a Python engine stood behind it.
In 3,360 probed shapes that was 207 answers. Now the operators check their
arguments as mongod 8.2.11 does, in mongod's order and with its codes and
messages, and compute what mongod computes: 3 refusals are left, all in
Decimal128 hyperbolic functions.

`$dateAdd` and `$dateSubtract` reach any date an int64 of milliseconds
holds, as mongod does, and accept a Timestamp or ObjectId start and a whole
Decimal128 amount; `$dateTrunc`, `$dateDiff` and `$dateToParts` take the
same dates. `$dateToString` evaluates `format`, honours `onNull` and renders
years 0-999. `$switch`, `$sortArray`, `$zip`, `$convert` and the regex
operators validate their arguments at parse time; a constant expression that
fails is refused while the pipeline is optimized, as mongod refuses it.

#### Fixed

- `$switch`: parse errors 40060-40068, and a missing branch is 40066 (40069
  when every case is a constant false, over an empty collection too) in
  `find`, `$match` and `$project` alike. Fixes RM-242
  (tests/backlog/test_rm_found_2026_10_21b.py::test_switch).
- `$dateAdd` / `$dateSubtract` / `$dateTrunc` / `$dateDiff` / `$dateToParts`:
  mongod's argument order and errors (5439013, 9, 5166403, 5166405,
  5976500 with mongod's per-unit amount limits, 5166406, 6045000, 5166307,
  5439017, 5439018, 5439006, 5439002, 40521, 16006), dates past year 9999,
  and a constant unit or bin size checked while optimizing. Fixes RM-250
  (tests/backlog/test_rm_found_2026_10_21b.py::test_date_arithmetic_arguments_and_range).
- `$dateToString`: `format` is an expression, `onNull` is honoured, years
  0-9999 render and a year component outside is 18537, a trailing `%` is
  18535. Fixes RM-247
  (tests/backlog/test_rm_found_2026_10_21b.py::test_date_to_string_arguments).
- `$sortArray`: parse errors 2942500-2942507, any number equal to 1 or -1 is
  a direction, a scalar under a field spec sorts as missing, a non-array
  input is 2942504. Fixes RM-241
  (tests/backlog/test_rm_found_2026_10_21b.py::test_sort_array).
- `$zip`: `defaults` evaluated, options checked (34462, 34463, 34466,
  34467). Fixes RM-240
  (tests/backlog/test_rm_found_2026_10_21b.py::test_zip_defaults_and_options).
- `$regexMatch` / `$regexFind` / `$regexFindAll`: `regex` and `options`
  checked before the input (51104-51110, flags `imsxu`). Fixes RM-238
  (tests/backlog/test_rm_found_2026_10_21b.py::test_regex_operator_options_are_validated).
- `$convert`: every BSON type is a target (an unreachable one is 241), a
  numeric `to` must be an int32 (9), a null `to` answers null, a missing
  `input` / `to` is a parse error. Fixes RM-228
  (tests/backlog/test_rm_found_2026_10_21.py::test_convert_to_a_target_it_cannot_produce)
  and part of RM-246
  (tests/backlog/test_rm_found_2026_10_21b.py::test_convert_targets_and_fallbacks).
- `$firstN` family with a NaN or Decimal128 `n`, `$toString` of year 1,
  `$meta: "randVal"`. Part of RM-252
  (tests/backlog/test_rm_found_2026_10_21b.py::test_operator_tail_codes_fixed).

#### Added

- Backlog RM-254: one stall of the backlog verifier in CI, unexplained.
