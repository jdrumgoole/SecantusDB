### The Rust MongoDB server's date operators read `timezone` as an expression

Every date operator on the Rust MongoDB server (`$hour` and the other
extractors, `$dateToString`, `$dateToParts`, `$dateFromParts`, `$dateAdd`,
`$dateSubtract`, `$dateDiff`, `$dateTrunc`) took its `timezone` argument as
literal text. So `{timezone: "$tz"}`, which reads the zone from each
document, was the unknown zone `"$tz"` and failed with 40485 on every
document. A computed zone (`{$ifNull: ["$tz", "UTC"]}`) was refused as
unsupported. All of them now evaluate it the way mongod 8.2.11 does.

#### Fixed

- **A `timezone` is evaluated.** A zone that evaluates to null or missing
  makes the result null. A non-string is 40517 (`timezone must evaluate to a
  string, found int`), prefixed by `$dateTrunc` and `$dateDiff` as mongod
  prefixes it. A constant zone is still checked while the pipeline is
  optimized and wins over the document's own errors; a computed one is
  checked per document, under the executor's wrapper.
- **Fixed offsets are mongod's.** `+05`, `-05`, `+24:00` and `+05:60` are
  zones, as mongod has them. The extractors and `$dateToString` refused
  them as unknown, though `$dateFromString` already accepted them.
- **`$dateToString`'s default format ends in `Z` only for UTC.** Under
  `GMT`, a named zone or a nonzero offset, mongod prints no `Z`.
- **`$dateFromParts` errors are mongod's.** A non-integer part is 40515
  (`'month' must evaluate to an integer, found string with value "x"`).
  `month`, `day`, `hour`, `minute`, `isoWeek` and `isoDayOfWeek` outside
  [-32768, 32767] are 31034; `year` out of 1 to 9999 is 40523 and
  `isoWeekYear` 31095. Mixing natural and ISO parts is 40489 or 40525.
  Decimal parts are accepted. The parts are read in mongod's order: the
  time of day, then the zone, then the date, and a null short-circuits
  what follows it. Before, most of these were BadValue (2) "not supported",
  and an out-of-range `month`, `day`, `hour` or `minute` answered a wrong
  date (`{year: 2017, month: 40000}` was 5350-04-01). A rollover past
  9999, which mongod allows, now gives a date instead of an error.

Fixes RM-139 (tests/backlog/test_rm_found_2026_10_14.py::test_date_from_parts_month_out_of_range_is_refused),
RM-126 (tests/backlog/test_rm_113_138.py::test_date_from_parts_error_codes,
tests/backlog/test_rm_found_2026_10_16.py::test_date_from_parts_errors_and_their_order)
and RM-173 (tests/backlog/test_rm_found_2026_10_16.py::test_a_date_operators_timezone_is_an_expression,
::test_fixed_offsets_mongod_accepts, ::test_date_to_string_default_format_drops_z_outside_utc).
Four engine-parity cases that pinned the deprecated Python engine's
wording were removed with a reason.
