### Intervals, and why they refuse to be one number

An interval is a duration, and PostgreSQL keeps it as three separate numbers:
months, days and microseconds. That looks like an implementation detail until
you try to collapse it. A month is 28 to 31 days depending on where you start,
and a day is 23, 24 or 25 hours across a daylight-saving boundary — so
`2026-01-31` plus one month is `2026-02-28`, a result no fixed count of
microseconds can express. Adding thirty days to the same date lands on March 2nd
instead, and both answers are correct for what was asked.

Comparison is the exception, and it goes the other way: PostgreSQL flattens the
parts using thirty-day months and twenty-four-hour days, so `'1 mon'` and
`'30 days'` compare *equal* while adding them takes you to different dates. The
Rust PostgreSQL server now does both — ordering through the flattened value,
arithmetic through the parts.

Intervals arrive in three written forms, all of which are now accepted: the
verbose one (`1 year 2 months`, with abbreviations and a `week` that becomes
seven days), a bare time (`02:03:04.5`, which carries its own sign and may run
past twenty-four hours), and ISO 8601 (`P1Y2M3D`, where `M` means months before
the `T` and minutes after it). They combine, and each part keeps its own sign:
`1 day -02:03:04` is a positive day and a negative time.

The output is PostgreSQL's, including the spelling that looks wrong: a value
pluralises whenever it is not exactly one, so `-1 day` prints as `-1 days`.

#### Added

- `interval` as a cast target, a literal and a bound parameter in both wire
  formats, with its own type oid.
- Interval comparison and ordering, flattened as PostgreSQL flattens it.
- `timestamp ± interval` with end-of-month clamping, `interval ± interval`, and
  scaling an interval by a number, where a fractional result spills from months
  into days and from days into time.

#### Fixed

- Beside an interval, a bare unquoted-type literal now resolves to an interval
  rather than to a timestamp, as PostgreSQL resolves it — so
  `'2020-01-01' + interval '1 day'` reports a bad interval instead of quietly
  doing date arithmetic, and `'1 day' + interval '1 day'` is two days.
