### Numeric division carries PostgreSQL's result scale

Dividing numerics now produces the display scale real PostgreSQL derives:
`SELECT 5.52 / 2.4` answers `2.3000000000000000` (scale 16), `1/3::numeric`
answers `0.33333333333333333333`, and a driver reading
`getBigDecimal().scale()` sees exactly what it would on Postgres. The rule is
`select_div_scale` from Postgres' own `numeric.c`, ported into the numeric
division path and verified against a live PostgreSQL 14.13 across twenty
division cases — every text render byte-identical. Values were already exact
after the numeric-exactness work; this closes the last recorded divergence,
the displayed scale. Integer division still truncates and float8 mixes still
coerce to float8, as before.

#### Fixed

- `secantus.sql`: `numeric / numeric` results are quantized to PG's derived
  division scale (`typemap.numeric_div`, half-away-from-zero rounding).
  Pinned by `tests/test_sql_numeric_div_scale.py` — a twenty-case battery
  whose expectations are byte-exact captures from PostgreSQL 14.13.
