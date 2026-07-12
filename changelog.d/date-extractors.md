### Six more date-component extractors and `$dateToParts` ISO mode

The aggregation date toolbox picks up the components MongoDB exposes but
SecantusDB was still missing: `$dayOfYear` (1-366), `$week` (US week number,
0-53, weeks starting Sunday), `$isoWeek` (ISO-8601 week 1-53), `$isoDayOfWeek`
(1=Monday … 7=Sunday), `$isoWeekYear` (the ISO week-numbering year), and
`$millisecond`. Each slots in alongside the existing extractors and accepts the
same two shapes — a bare date expression or a `{date, timezone}` object — so a
fixed `±HH:MM` offset or a named IANA zone (`America/New_York`) shifts the
instant before the component is read. The year-boundary edge cases match
mongod: `2026-01-01` (a Thursday) is US week 0, and `2027-01-01` (a Friday) is
ISO week 53 of ISO year 2026.

`$dateToParts` now honours `iso8601: true`, returning `{isoWeekYear, isoWeek,
isoDayOfWeek, hour, minute, second, millisecond}` instead of the calendar
`{year, month, day, …}` shape. The `timezone` option applies in both modes, and
`iso8601: false` (or absent) keeps the existing output unchanged.

Both servers gain the operators together, pinned byte-for-byte by the Rust ↔
Python expression parity harness. The named-IANA-zone cases compute natively on
the Rust side via `chrono-tz`.

#### Added

- `expressions.py` / `secantus-core`: `$dayOfYear`, `$week`, `$isoWeek`,
  `$isoDayOfWeek`, `$isoWeekYear`, and `$millisecond` aggregation-expression
  operators, each supporting the `{date, timezone}` object form.
- `expressions.py` / `secantus-core`: `$dateToParts` now supports
  `iso8601: true`, emitting the ISO week-based parts document.
