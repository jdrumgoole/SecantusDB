### JDBC clients get their real time zone

A pgjdbc connection tells the server its JVM time zone through a `TimeZone`
**startup parameter** — and the PG server dropped it, leaving every JDBC
session on UTC. For clients west of Greenwich that shifted date reads back a
day (`1950-02-07` came back `1950-02-06`). Startup GUC parameters are now
applied and echoed in the opening ParameterStatus burst, the way PostgreSQL
treats them.

Four smaller conformance gaps closed with it: `SET timezone = 'gmt-3'` now
reports the normalized `GMT-3` spelling (pgjdbc's ParameterStatus parser is
case-sensitive and silently fell back to UTC on the lowercase echo);
POSIX-style zone specs accept minutes (`GMT+3:30` is UTC-03:30, pgjdbc's
half-hour-zone test); `tstz::text` casts render the session-zone offset and
`tz::text` renders PostgreSQL's `+01` spelling; and a BC-era timestamptz
literal without an offset is stamped with the session zone's offset so the
stored instant is correct. pgjdbc's TimezoneTest is now **16/16** and
DateTest **192/192** — and this time measured with a fixed tally (the
release-note claim that DateTest was already clear traced to an XML-parsing
bug in the measurement script, not the server).

#### Fixed
- `TimeZone` (and other reportable GUCs) sent as startup parameters are
  applied to the session and reported in the initial ParameterStatus burst.
- `TimeZone` values normalize to PostgreSQL's reported spelling
  (`gmt-3` → `GMT-3`).
- POSIX GMT/UTC offsets accept minutes and seconds (`GMT+3:30`).
- `timestamptz::text` renders the session-zone offset; `timetz::text`
  renders whole-hour offsets as `+01`.
- An out-of-range (BC) timestamptz literal without an offset takes the
  session zone's offset instead of UTC.
