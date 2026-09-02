### `numeric(p, s)` never applied its declared scale

PostgreSQL **rounds** a stored value to the declared scale: `0.12345` into a
`numeric(10,3)` column is stored as `0.123`, and a bare `1` as `1.000`. This
engine kept whatever scale the literal happened to carry, so the *stored value
itself* was wrong — not merely its rendering — and every `sum`, `min` / `max`,
`avg` and arithmetic result over the column inherited the error.

`sum(a)` over 1, 2.5 and 0.12345 in a `numeric(10,3)` column answered `3.62345`
where PostgreSQL answers `3.623`.

#### Fixed

- A declared `numeric(p, s)` rounds to `s` on write — `INSERT`, `UPDATE`,
  parameterised statements and `COPY` alike — half away from zero, matching
  PostgreSQL both signs.
- A value whose integer part does not fit after rounding is `22003 numeric
  field overflow`, with PostgreSQL's DETAIL line naming the precision, the
  scale and the limit. The check runs on the **rounded** value, so `9999.999`
  into a `numeric(6,2)` overflows.
- `NaN` and NULL are stored untouched, and an unconstrained `numeric` keeps its
  own scale, as PostgreSQL does.
