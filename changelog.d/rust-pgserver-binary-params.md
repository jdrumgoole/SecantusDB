### Bound parameters in the binary format

Client libraries do not send parameters as text. psycopg, like most modern
drivers, sends numbers, dates, timestamps and arrays in PostgreSQL's binary
format by default, and falls back to text only where it must. The Rust
PostgreSQL server decoded integers, floats, booleans and strings that way and
refused everything else — so binding a `Decimal`, a `date`, a `datetime` or a
list failed, even though the same values written as SQL literals worked.

Binary decoding now covers `numeric`, `date`, `time`, `timestamp`, and arrays
of every element type the server knows. Each one decodes to the same canonical
text a literal would have, so a bound value takes exactly the same path through
the planner as a written one; the alternative — a second, parallel set of
conversions for the binary format — is how the two formats drift apart and
start disagreeing about the same value.

The text format had a bug of its own that this work surfaced. A `numeric`
parameter was being parsed as a floating-point number, so a client binding
`1.50` got back a float that had already lost both the exactness and the scale
that make it a different value from `1.5`.

Separately, a timestamp *constant* answered NULL. A stored timestamp is
reassembled from its column plus a hidden field carrying sub-millisecond digits,
and a constant never passes through a row — so it reached the encoder in a shape
nothing matched, while the identical value read from a column, or cast to text,
came back correctly. Three routes to the same value, one of them silently empty.

#### Added

- Binary-format decoding for bound `numeric`, `date`, `time`, `timestamp` and
  array parameters, including NULL elements and empty arrays.

#### Fixed

- A `numeric` parameter sent as text was parsed as a float, losing exactness and
  scale.
- `SELECT '2026-01-01 12:00'::timestamp` answered NULL, though the same value
  through a table column or cast to text was correct.

#### Changed

- A multidimensional array sent as a binary parameter is refused with `0A000`
  rather than decoded, matching what the server does when returning one.
