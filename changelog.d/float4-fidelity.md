### Single-precision float fidelity

float4 values now behave like PostgreSQL's: a cast to float4 narrows the
value to single precision, and its text form is the shortest decimal that
round-trips at that precision — `(1/3.0)::float4` prints `0.33333334`, in
arrays too — while float8 keeps the full double form. The
`extra_float_digits` GUC's negative range now works for both widths
(`%.{15+n}g` / `%.{6+n}g`, as PG renders when shortest-output is turned
down), which also uncovered that `SET` silently dropped the minus sign from
negative values. Bare `ARRAY[…]` and `ROW(…)` constructors now name their
output columns `array` and `row` like PG. The pgtest `float` corpus file
pins all of it and is now green.

#### Fixed
- float4 rendered at double precision (`0.3333333333333333` instead of
  `0.33333334`).
- `SET guc = -1` stored `1` — the sign vanished in value extraction.
- Unaliased ARRAY/ROW constructor columns were named `?column?`.
