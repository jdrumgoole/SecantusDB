### `timestamp with time zone` respects the session's time zone

A value written without an offset — `'2005-01-01 12:00:00'` — was read as UTC
rather than as local time in the connection's own time zone, so it was stored
at the wrong instant by however far that zone sits from Greenwich. Reading it
back showed the same skew, which for a value near midnight moved it to the
previous or the following day.

Such a value is now interpreted in the session's zone, as Postgres does, and
displayed back in that zone. A value that arrives carrying its own offset is
already unambiguous and is left alone.

Two smaller things came with it. Zone names written with an offset, like
`GMT+13`, previously resolved to nothing and fell back to UTC; they now resolve,
keeping the POSIX convention Postgres follows where `GMT+13` means thirteen
hours *behind* UTC. And offsets are written the way Postgres writes them — `+00`
and `-05` rather than `+00:00`, widening to `+05:30` only where the minutes
matter — which clients that compare the rendered text depend on.

Values of type `date` and `timestamp without time zone` are unaffected, as they
should be: neither has an instant behind it to move.

#### Fixed

- A `timestamptz` written without an offset is interpreted in the session's
  time zone instead of UTC, and displayed in that zone.
- Zone settings of the form `GMT±N` resolve, with Postgres' sign convention.
- Offsets render in Postgres' spelling.
