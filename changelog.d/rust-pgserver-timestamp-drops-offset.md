### Rust pgserver: a timestamp drops a trailing time-zone offset

PostgreSQL's `timestamp` (WITHOUT time zone) input accepts a trailing offset
and drops it, keeping the wall-clock reading — `'2000-01-01 03:02:03+02'` is
`2000-01-01 03:02:03`. The Rust server rejected the offset (`22007` / `22008`),
which surfaced when psycopg dumps a tz-aware datetime that reaches the naive
timestamp parser (its offset now reflects the session zone, so it can even be a
second-precision `-01:02:03`).

#### Fixed
- `timestamp` input now accepts and drops a trailing `±HH[:MM[:SS]]` offset,
  matching PostgreSQL.
