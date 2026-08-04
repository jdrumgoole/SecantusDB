### A date written with a time-zone offset and no clock time

`1950-02-07 -05` — a calendar date, an offset, and no time of day — is what a
JDBC client sends for a date when it has been given a calendar. We read the
offset as though it were the time itself, so the value quietly became five in
the morning with no zone at all, and a `timestamp` column stored it that way.

Postgres reads the implicit midnight, and so do we now: the date lands on the
day it names, a `timestamp` column keeps midnight, and a `timestamp with time
zone` column keeps the instant that midnight refers to.

Dates at the very edge of the representable range are handled alongside this.
Now that the offset is understood, shifting one of those to UTC can fall off
the end of the calendar — the first instant of year 1 is in year zero once you
move it west. Those keep their clock face rather than failing.

#### Fixed

- A date literal carrying a time-zone offset but no time of day is read as
  midnight at that offset, rather than as a time.
