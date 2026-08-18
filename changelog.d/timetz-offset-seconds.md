### A time zone offset may carry seconds

Before the world agreed on standard time, local clocks ran on local mean solar
time, and those offsets were rarely a whole number of minutes — Dublin sat at
-00:25:21, Amsterdam at +00:19:32. Zone databases still carry them, so
Postgres accepts and preserves a UTC offset with seconds in it. SecantusDB
rejected such a literal outright with `22007 invalid input syntax for type
timetz`, which meant any value drawn from one of those historical zones could
not be stored at all.

`timetz` now accepts an offset carrying seconds and renders it the way
Postgres does, which is not simply "print what you were given": the seconds are
kept when non-zero — even when the minutes are zero, so `+01:00:03` stays wide
— then dropped when zero, after which a zero minutes field is dropped too. So
`+01:01:03` round-trips intact, `+01:01:00` narrows to `+01:01`, and
`+01:00:00` narrows to `+01`. A cast to plain `time` still discards the whole
zone, seconds included. Every one of those spellings was probed against a real
PostgreSQL 14 rather than inferred.

#### Fixed

- A `timetz` literal whose UTC offset carries seconds (`'00:00:00+01:01:03'`)
  is accepted and preserved instead of raising `22007`.
