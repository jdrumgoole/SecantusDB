### `$dateFromString` with a `format` now parses the way mongod does, on the Rust server

`$dateFromString` with a `format` on the Rust MongoDB server is now a literal
port of the parser mongod uses: timelib's parse-from-format, run under mongod's
own table of format specifiers and its `%` prefix. Before, it used a
hand-written strptime. That refused most of what mongod accepts (`%L`
milliseconds, ISO weeks, `%z` zones, and every malformed string) and got some
answers wrong. `%j` came out a day early, because mongod counts the day of the
year from zero, and a lone `%Y` returned a date where mongod reports the string
as incomplete.

Against mongod 8.2.11, 433 date-string cases (now including 115 with a
`format`) give 0 divergences, in both the values and the full error text.

#### Fixed

- `$dateFromString` with a `format` on the Rust server: `%L`, `%G` / `%V` /
  `%u`, `%z`, `%Z` (an offset in minutes), `%b` / `%B` and `%%` all parse.
  `%j` is zero-based. Invalid dates and times, trailing data, missing data and
  literal mismatches return mongod's 241 message, with timelib's positions and
  characters.
- A format with an unknown specifier or a trailing `%` now returns mongod's
  18536 / 18535. A non-string `format` returns 40684, and a non-string
  `timezone` returns 40517. These checks run in mongod's order, so a bad
  format is reported even when `dateString` is null.
- `format` and `timezone` are evaluated as expressions, so `"$field"`
  references work.
- A non-string `dateString` is a 241 that `onError` catches, as on mongod. The
  Rust server used to refuse it.
