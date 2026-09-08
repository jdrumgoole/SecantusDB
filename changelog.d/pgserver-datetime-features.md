### Datetime arithmetic and special values on the Rust PostgreSQL server

The Rust `secantusd-pg` server now speaks the everyday datetime idioms a
PostgreSQL client reaches for. `date + int`, `date - date`, `timestamp +
interval`, `interval + interval` and `interval * n` all evaluate — and, just as
importantly, describe their result column with PostgreSQL's own type, so a
client that picks its loader from the row description (psycopg does) decodes a
`timestamp + interval` as a timestamp rather than as text. The `epoch` special
literal is accepted on `date` and `timestamptz` alongside the `timestamp` it
already handled, and `24:00:00` is recognised as PostgreSQL's valid end-of-day
`time`.

Out-of-range results are rendered in PostgreSQL's own text — a year past 9999,
or the `BC` era — so the client's own loader is what rejects a value it cannot
hold, exactly as it does against a real server. This closes 38 of the remaining
cases in psycopg's vendored `test_datetime.py` suite; the only ones left need a
full IANA time-zone database (named-zone DST result loading), which stays out of
scope.

#### Added

- `crates/secantus-pgplan`: `date +/- int`, `int + date`, and `date - date`
  (→ `int4`) arithmetic; the `epoch` literal on `date` and `timestamptz`; and
  `24:00:00` as a valid end-of-day `time`.

#### Fixed

- `crates/secantus-pgplan`: datetime arithmetic (`timestamp + interval`,
  `interval + interval`, `interval * n`, …) is now typed from its operands, so
  the result column is described with the correct OID even at DESCRIBE time when
  every value is NULL — previously it was described as `text`/`int4` and clients
  decoded it wrongly, and interval / timestamp / date overflow was never
  surfaced to the client's loader.
