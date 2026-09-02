### A fourth sweep: two badly wrong answers, three wrong types

25 of 35 shapes matched PostgreSQL 14.13 across windows, aggregates, bytea and
subqueries.

`substring(b from 1 for 1)` over a `bytea` answered the string `'b'` — the
first character of the Python repr `b'\x01\x02'` — where PostgreSQL answers the
byte `\x01`.

`every(n > 5)` answered NULL. `every` is the standard-SQL spelling of
`bool_and`, and the mapping kept the name but dropped an *expression* argument;
`bool_and(n > 5)`, the same aggregate, was right all along.

#### Fixed

- `substring` / `substr` over `bytea` slice bytes and report `bytea`.
- `every(<expression>)`.
- A scalar subquery takes its projection's type: `(SELECT count(*) FROM t)` is
  `bigint`, and a correlated aggregate takes its column's type. It was `text`,
  so a driver was sent the string `'3'` under oid 25.
- A `::numeric(p, s)` **cast** rounds to its declared scale, as the column path
  already did — `10::numeric(5,2)` is `10.00`, and a value that no longer fits
  is `22003`.
- `round()` / `floor()` / `ceil()` report their argument's numeric type;
  `round(2.345::float8)` claimed `numeric` for a float result.
- `GROUPS` window frames, whose offset counts **peer groups** rather than rows.
  They were not handled at all — they fell through to the `RANGE` branch and
  reported `RANGE with a numeric offset requires a numeric ORDER BY key`, an
  error about a clause the user had not written.

#### Still divergent

`sqrt(numeric)` returns `float8` (PostgreSQL returns numeric at a scale its
`sqrt_var` estimator picks), `corr` / `covar_pop` / `regr_*` are unsupported,
`min`/`max` accept a `bytea` PostgreSQL rejects, and a scalar subquery under
`GROUP BY` cannot be typed because the synthetic resolver cannot see the inner
table.
