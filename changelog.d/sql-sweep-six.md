### A sixth sweep: `age()` borrowed from the wrong month

20 of 33 shapes matched PostgreSQL 14.13 across constraints, identity columns,
time zones, GUCs and string functions. The important miss was silently wrong
arithmetic.

`age('2021-03-15', '2020-01-20')` answered `1 year 1 mon 23 days` where
PostgreSQL answers **26 days**. When the day difference goes negative, the
borrow takes the length of the **start** date's month — January's 31 here — not
the month before `end` (February's 28), and not a flat 31. Eight probed cases
discriminate all three readings, including `age('2020-04-01','2020-01-15')`
(January's 31, not April's 30) and `age('2020-03-01','2020-02-28')` (February's
29, not 31).

#### Fixed

- `age()`'s day borrow. Sixteen cases now match, and two existing tests that
  had recorded the old answer are corrected against the reference server.
- `format('%1$s-%1$s-%2$s', 'a', 'b')` — positional argument specifiers, which
  may repeat one. Unrecognised, the whole directive was copied through as
  literal text, so the format string came back unformatted.
- `current_setting('nope', true)` is NULL and `current_setting('nope')` is
  `42704 unrecognized configuration parameter`. Both answered the empty string,
  which reads as a setting that exists and is blank.
- `parse_ident()`, `unistr()` and `normalize()`.
- `localtimestamp` nested in an expression, and in the **session's** time zone.
  It used the machine's wall clock, a different instant whenever the two zones
  differ — with the default UTC session on a UTC+1 host,
  `localtimestamp <= now()` was FALSE.

#### Still divergent

`AT TIME ZONE`, `extract(timezone_hour …)`, `LIKE ALL/ANY(array)`, and
`to_ascii()`.
