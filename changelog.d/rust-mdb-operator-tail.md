### Rust MongoDB server: more expression arguments read as mongod reads them (RM-246, RM-252, RM-255 to RM-257)

A fourth pass over the expression harness's divergences, each measured
against mongod 8.2.11. In the 3,360-shape harness:
- wrong error codes fell from 63 to 13;
- wrong values fell from 28 to 24;
- message-only differences fell from 35 to 27.

No shape newly diverged.

#### Fixed

- `$convert` / `$toInt`: a double past int32's range is 241, judged on the
  double before it truncates, so 2147483647.5 no longer clamps. A string
  below the smallest normal double is 241 "Out of range", as `strtod`'s
  ERANGE makes it. Fixes RM-246
  (tests/backlog/test_rm_found_2026_10_21b.py::test_convert).
- `$arrayElemAt` is null for a null or missing operand before either is
  checked, and checks the array before the index. `$getField` of `$$REMOVE`
  is missing. `$setField` / `$unsetField` with a field path or system
  variable for `field` is 4161108, rendered as mongod renders it.
  `{$meta: "sortKey"}` is missing in a query filter's `$expr`, `$match` and
  `$redact`, after a `$sort` too, and `[]` in a `$group` with no sort.
  `$type` of anything that evaluates to missing is "missing". Fixes RM-252
  (tests/backlog/test_rm_found_2026_10_21b.py::test_operator_tail_codes,
  ::test_operator_tail_codes_fixed).
- `$dateToString` reads `format` before the date. A bad directive or a
  non-string format fails beside a null date, and a null format is null
  beside a date that is not one. Part of RM-251
  (tests/backlog/test_rm_found_2026_10_21b.py::test_date_to_string_reads_format_first).
- `$substrBytes` / `$substr` coerce a Decimal128 index as Decimal128
  converts to a long: rounded half to even, and INT64_MIN for a value no
  long holds. As a start that gives 50752; as a length it means "to the
  end". Fixes RM-255
  (tests/backlog/test_rm_found_2026_10_21b.py::test_substr_bytes_decimal_indices).
- `$dateDiff` / `$dateTrunc` read `startOfWeek` only for the `week` unit.
  In an expression that reads the document, a constant `startOfWeek` is
  still parsed while optimizing: a bad one fails, and a null one makes the
  answer null. Fixes RM-256
  (tests/backlog/test_rm_found_2026_10_21b.py::test_start_of_week_only_matters_for_weeks).
- `$percentile`'s `p` must be a constant expression, and is evaluated:
  - `$literal`, `$concatArrays` and constant elements count;
  - a field path is 7750300;
  - NaN and Decimal128 elements are accepted. A NaN `p` crashes mongod
    8.2.12 on Linux x86-64 (an out-of-bounds read after casting NaN to an
    index); arm64 answers the lowest value, and so does this server.

  A non-string `method` is 14 before anything else, and a null one is
  missing. The command layer's parse-time check now calls the same
  validator. Fixes RM-257
  (tests/backlog/test_rm_found_2026_10_21b.py::test_percentile_p_is_a_constant_expression).

#### Filed

- RM-258: `$dateFromString` of a five-digit year is ten seconds off from
  mongod's timelib (`"10000-01-01"`).
