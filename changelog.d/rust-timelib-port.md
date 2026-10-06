### Rust MongoDB server: timelib's date parser, ported

`$toDate` and `$dateFromString` on the Rust MongoDB server now parse date
strings with a port of timelib, the same parser mongod uses, so they match
mongod 8.2.11 exactly, error messages included.

#### Changed

- Free-form date strings go through a literal port of timelib 2022.13's
  scanner and post-processing, and of mongod's wrapper that turns its errors
  into messages. On 318 probed cases, values and full error text match
  mongod: every error and warning, with the position and character mongod
  reports. Before this, 26 strings got only a generic "incomplete date/time"
  message.
- The zone abbreviation table is generated from mongod's own copy of timelib's
  `timezonemap.h`, which has 1,127 entries.

#### Fixed

- `$dateFromString` without a `format` refused every string mongod parses
  except strict ISO-8601.
- `$dateFromString`'s `onError` was never applied to a string that failed to
  parse.
- With a named `timezone`, a local time in a daylight-saving overlap resolved
  to the earlier instant, and one in a daylight-saving gap was an error.
  Both now resolve as on mongod.
- `$toDate` and `$dateFromString` refused dates outside the years 1–9999.
  mongod accepts any 64-bit millisecond value, such as year 0 or year
  −100000, and reports an overflow only at its own limit (code 159).
