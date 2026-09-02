### `to_char` on an interval crashed, and day names ignored their case and padding

`to_char(interval '3 days', 'DD')` reported an internal error — `to_char`
assumed a date, and an interval is stored differently, so it fell through to
the date parser. Interval templates now work: `DD`, `HH24`, `MI`, `SS`, `MM`,
`YYYY` and combinations of them. Calendar-name templates like `Day` are
rejected with PostgreSQL's own message, since an interval is not tied to a
calendar date.

`to_char(date, 'Day')` returned `Thursday` where PostgreSQL returns
`Thursday ` — the full day and month names are padded to nine characters — and
`DY` returned `Thu` rather than `THU`. The token's own spelling decides both
the padding and the capitalisation, and neither survived the conversion the
formatter was doing.

#### Added

- `to_date()` and `to_timestamp()`, in both the format-string and
  epoch-seconds forms. They previously reported that a function called
  `str_to_date` was unsupported — a name the query never used.
- `extract(century …)`, `extract(millennium …)` and `extract(decade …)`.

#### Fixed

- `to_char()` on an interval.
- Day and month names pad to nine characters, and follow the case of the
  template (`DAY` upper, `Day` capitalised); `FM` suppresses the padding.
- `to_date()` reports a date and `to_timestamp()` a timestamp, rather than
  text.

#### Known limitation

An all-lower-case `day` or `dy` template renders capitalised. The parser maps
the leading `D` before we see it, so `day` and `Day` are indistinguishable by
then; `DAY` and `DY` are unaffected.
