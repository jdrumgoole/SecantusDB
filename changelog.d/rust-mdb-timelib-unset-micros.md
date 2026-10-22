### Rust MongoDB server: a no-colon time is ten seconds early, as on mongod

mongod's timelib reads a four-digit time without a colon (`"2026-01-01
1030"`, `"10000-01-01"` as `1000` and `0-01-01`) through its `gnunocolon`
rule. That rule sets the hours and minutes but leaves the microseconds
unset, which timelib represents as -9999999. mongod zeroes them only when
the whole time is missing, then adds them as they are. So every such time
is 9.999999 seconds early, truncated to ten: `"2026-01-01 1030"` is
10:29:50. The Rust port of timelib read the unset microseconds as 0. It now
adds them as mongod does. This was traced by compiling mongod 8.2.11's own
timelib with its parser trace.

#### Fixed

- `$dateFromString` of a no-colon `HHMM` time, with or without a timezone,
  matches mongod (32 of 32 measured shapes). Times with a colon, with
  seconds (`HHMMSS`) or parsed through a `format` were already right and
  are unchanged. Fixes RM-258
  (tests/backlog/test_rm_found_2026_10_21b.py::test_date_from_string_five_digit_year).
