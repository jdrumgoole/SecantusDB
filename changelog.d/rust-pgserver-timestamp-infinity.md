### Rust pgserver: infinity, epoch and out-of-range dates

PostgreSQL's date and timestamp domain is far wider than a Python `date` or
`datetime`: `infinity` / `-infinity` are real values, and so are years past
9999 and dates BC. The Rust PostgreSQL server now accepts all of them —
storing the canonical text and letting the client's loader decide what it can
hold, exactly as a real server does (psycopg's overflow tests want the "date
too large" the loader raises, not a server error). A wide-year or BC plain
`timestamp` is rendered the way PostgreSQL renders it, with the time
canonicalised to `HH:MM:SS`. The one constant special input keyword, `epoch`,
now resolves to `1970-01-01 00:00:00`.

#### Added
- `infinity` / `-infinity`, years > 9999, and BC dates are accepted on `::date`
  and `::timestamp` casts and passed through as canonical text.
- `'epoch'::timestamp` resolves to `1970-01-01 00:00:00`.

#### Fixed
- A wide-year (> 9999) or BC date no longer errors on the Rust pgserver; a
  datetime-shaped value with an impossible field is `22008` and a non-date is
  `22007`, matching PostgreSQL.
