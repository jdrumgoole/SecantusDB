### The Rust PostgreSQL server keeps `numeric` exact past 34 digits

PostgreSQL's `numeric` is arbitrary precision; the Rust PostgreSQL server
stored it as Decimal128, which holds 34 significant digits and an exponent no
wider than ±6144. A value beyond that was refused with a `22003` where
PostgreSQL simply returns it — a loud refusal, but psycopg's exhaustive numeric
round-trip tests, and any application that keeps a 40-digit key or a
1000-digit constant, could not run at all.

Wide values now persist as their canonical PostgreSQL text alongside a
byte-sortable key, and everything that fits Decimal128 stays Decimal128. The
two forms compare and sort by value — `1.50` still ties `1.5`, a 40-digit
value still lands after a 35-digit one, and `NaN` takes PostgreSQL's place
above infinity — in every WHERE operator, in ORDER BY, through a numeric
PRIMARY KEY, and inside `sum` / `min` / `max`. Arithmetic is exact at any
width, with PostgreSQL's division-scale rule, and a computed numeric column
is described as `numeric` rather than as an integer. Every expectation was
measured on PostgreSQL 16.

#### Fixed

- `secantus-pgplan/src/numeric.rs` (new): canonical-text parsing and
  rendering, the `{__numeric, __numkey}` wide representation, `Decimal128`
  bracketing, `numeric_filter` WHERE lowering (Decimal128 arm + key arm + NaN
  arm), exact `BigInt`-backed `+ - * /`, `sum_numeric_texts`, and a value
  comparison that ranks `NaN` above `Infinity` as PostgreSQL does.
- `secantus-pgplan`: `sum(numeric)` is typed `numeric`; a computed numeric
  column (`n * 2`) is `numeric` (oid 1700), not `int4` — the client's integer
  loader used to choke on `3.0`.
- `secantus-pgserver`: text and binary encoders, array elements, record
  fields, parameter decoding and casts all accept and emit wide values; `sum`
  over numeric is exact; ORDER BY and every comparison use the value order; a
  numeric PRIMARY KEY rejects a duplicate that differs only in display scale
  (`1e40` vs `1e40.0`) with `23505`, and resolves equality / range / UPDATE /
  DELETE by value through the `_id` index.
