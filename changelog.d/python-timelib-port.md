### The Python server reads date strings with mongod's own parser

The Python MongoDB server now parses date strings the way mongod 8.2.11 does,
because it runs the same parser: a pure-Python port of timelib 2022.13 -- the
library mongod vendors -- together with mongod's own wrapper around it. That
covers `$dateFromString` with and without a `format`, `$toDate`, and
`$convert` to a date. Before this, the Python server used Python's ISO parser
and `strptime`, and on mongod's differential probe of 433 date strings it
answered 323 differently: wrong values (every 12-hour time, every zone
abbreviation), strings mongod accepts refused, and error text that matched
nothing mongod says. It now matches on all 433, values and full error text.

#### Fixed

- `secantus/timelib/` (new): a literal port of timelib's free-form scanner
  (`parse_date.re`, re2c longest-match semantics), its parse-from-format under
  mongod's `kDateFromStringFormatMap`, `timelib_update_ts`, and the 1,127-entry
  zone abbreviation table (generated from mongod's `timezonemap.h` by
  `tools/timelib/gen_zone_tables.py --python`). Pure Python, no Rust in the
  request path.
- `$dateFromString`: evaluated in mongod's order -- `format` type (40684) and
  validity (18535 / 18536) first, then `timezone` (40517 / 40485), then
  `onNull`, then `onError` around the `dateString` type check and the parse
  (241, now "found: int with value 5"). `%j` is zero-based as in mongod; `%y`
  is refused (not a mongod specifier); a lone `%Y` / `%j` is
  incomplete; a zone in the string together with a `timezone` argument is
  refused; a DST gap or overlap resolves the way mongod's does; a present but
  null `format` gives null; and a literal bad `format` is reported before a
  bad literal `timezone`.
- `$toDate` / `$convert` of a string: every shape timelib accepts (relative
  forms, `@` timestamps, ISO weeks, month names, zone abbreviations, 12-hour
  times) with mongod's value, and mongod's exact error text otherwise. Dates
  outside years 1-9999 are returned as raw BSON dates rather than refused; one
  beyond about +-292,000 years is mongod's 159 `DurationOverflow`.
