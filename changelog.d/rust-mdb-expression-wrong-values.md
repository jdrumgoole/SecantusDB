### Rust MongoDB server: six expressions that answered a different value from mongod

Six operators gave answers that differed from mongod 8.2.11's without any
error to say so. Regex classes matched Unicode where mongod's PCRE matches
ASCII, `$regexFindAll` counted empty matches differently, `$filter` ignored
or misread its `limit`, `$round` and `$trunc` rounded a double's decimal
text instead of its binary value, `$pow` turned `1 ^ -5` into a double, and
local times at a daylight-saving change resolved to the wrong instant. In
the 3,360-shape expression harness, wrong values fell from 98 to 34 and
wrong error codes from 94 to 63, with no shape newly diverging.

#### Fixed

- Regex `\w`, `\d`, `\s`, `\b` and `\B` match ASCII only, in a bracket class
  too, with or without the `u` flag, in `find` `$regex` and the three regex
  operators. `\s` includes VT, but not U+0085 or a no-break space, and `\w`
  under `i` does not reach the Kelvin sign. Fixes RM-236
  (tests/backlog/test_rm_found_2026_10_21b.py::test_regex_classes_are_ascii).
- `$regexFindAll` scans the way mongod does. It finds no empty match at the
  end of the string, it counts an empty match straight after a non-empty
  one (`a*` over "abc" is "a", "", ""), and it still gives an empty input
  one match. Fixes RM-237
  (tests/backlog/test_rm_found_2026_10_21b.py::test_regex_find_all_has_no_trailing_empty_match).
- `$filter`'s `limit`: an integral double or Decimal128 counts as a limit,
  zero or a negative value is 327392, and anything that is not an int32 is
  327391. An empty input returns before the limit is checked. Fixes RM-239
  (tests/backlog/test_rm_found_2026_10_21b.py::test_filter_limit).
- `$round` / `$trunc` of a double round through a 34-digit Decimal128, as
  mongod does, so `$round: [2.675, 2]` is 2.67 and `$round: [1e300, 10]` is
  1e300 rather than Infinity.
  - A null value is null before the place is checked, and a non-number is
    51081 whatever the place.
  - A Decimal128 NaN place is 51082.
  - An int32 result widens only past int32's top; below its bottom it
    wraps, as on mongod.

  Fixes RM-243
  (tests/backlog/test_rm_found_2026_10_21b.py::test_round_and_trunc_of_doubles).
- `$pow` of 1 or -1 to a negative integer stays an integer. Fixes RM-244
  (tests/backlog/test_rm_found_2026_10_21b.py::test_pow_of_one_with_negative_exponent_stays_integral).
- Local times at a daylight-saving change, in `$dateFromParts`, `$dateAdd`,
  `$dateTrunc` and `$dateFromString`:
  - a time that happens twice is the later, standard-time instant;
  - `$dateAdd` and `$dateSubtract` instead keep the start date's daylight
    saving;
  - a time that never happens is read with the offset in force before the
    change, where it used to be 40485 "unrecognized time zone identifier".

  Fixes RM-248
  (tests/backlog/test_rm_found_2026_10_21b.py::test_local_times_at_daylight_saving_changes).
