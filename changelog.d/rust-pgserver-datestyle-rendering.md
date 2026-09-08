### Rust pgserver: honour DateStyle in date/time text output

The Rust PostgreSQL server (`secantusd-pg`) now renders `date`, `timestamp` and
`timestamptz` values in the session's `DateStyle` — the four display formats
(`ISO`, `Postgres`, `SQL`, `German`) crossed with the `YMD` / `MDY` / `DMY`
field order — instead of always rendering ISO. A `SET datestyle = SQL, DMY`
followed by `SELECT '2026-09-08'::timestamp` now answers `08/09/2026 12:34:56`,
byte-for-byte what PostgreSQL 14 answers, and `Postgres` style gains the
spelled-out day-of-week and month (`Tue Sep 08 12:34:56.789 2026`) it requires.

Because the output now genuinely respects the style, the server once again
reports `DateStyle` over `ParameterStatus` (in PostgreSQL's canonical spelling)
— which a previous change had to disable, since reporting a style the output
ignored made psycopg switch its parser to a layout the server never produced and
mis-parse every datetime. Reporting is safe now that the two agree: psycopg's
loaders and the server's output speak the same style. Binary datetime output is
DateStyle-independent and is left untouched.

#### Added
- `secantus-pgplan`: `DateStyle` (format + field order) with `parse` /
  `canonical`, and `render_date_styled` / `render_timestamp_styled` /
  `render_timestamptz_styled` (plus the `*_value_text_styled` Bson helpers).

#### Fixed
- `secantus-pgserver`: `date` / `timestamp` / `timestamptz` TEXT output renders
  in the session `DateStyle`; `SET datestyle` is stored and reported in
  canonical form. Fixes the 12 psycopg `test_overflow_message[timestamptz-*]`
  cases (a non-ISO style must make the driver's timestamptz loader raise
  `NotImplementedError`).
