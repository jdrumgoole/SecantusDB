### Rust PG server: a timestamp, interval or bytea DEFAULT reads back as a literal

PostgreSQL prints a column's constant DEFAULT through the type's output
function, quoted and labelled: `'2020-01-01 00:00:00+00'::timestamp with
time zone`, `'1 day'::interval`, `'\x01'::bytea`. The Rust PostgreSQL server
printed the stored value's internal form instead, so
`information_schema.columns.column_default` and `pg_get_expr` over
`pg_attrdef` showed `DateTime("2020-01-01 0:00:00.0 +00:00:00")`, an
interval document or `Binary(0x0, AQ==)` -- text that a migration tool
comparing defaults, or a schema dump, cannot read back. It now prints the
literal PostgreSQL does, in the reading session's DateStyle, TimeZone,
IntervalStyle and bytea_output.

#### Fixed

- A `timestamp`, `timestamptz`, `interval` or `bytea` column's DEFAULT is
  the output function's text, quoted and labelled with the type, in
  `information_schema.columns.column_default` and `pg_get_expr(adbin,
  adrelid)`. A `timestamptz` is written in the session zone, and every
  type follows the session's output settings, as `get_const_expr` does.
- A `date` DEFAULT follows the session DateStyle (`'02.01.2020'::date`
  under German).

Fixes RP-864 (tests/backlog/test_rp_found_rp846.py::test_a_temporal_or_bytea_default_prints_as_a_literal).

Files RP-894: a `date` or `timestamp` made text inside an expression
(`concat`, `format`, a record's text) is still ISO under every DateStyle.
