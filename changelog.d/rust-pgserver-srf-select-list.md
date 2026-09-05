### Set-returning functions where clients actually put them

`select generate_series(1, 10)` — the function in the select list, with no
`FROM` at all — now returns ten rows rather than an error. It is the shortest
way to conjure rows without a table, and it is what client test suites reach for
constantly; a server cursor over exactly that form was the single most common
use in psycopg's.

It is planned as an ordinary select over a generated source, which is the same
shape the `FROM generate_series(...)` form already produced. That means ordering,
limits and offsets came along for free rather than being written a second time
for a second syntax that means the same thing.

A set-returning function *beside* another output column — `select 1,
generate_series(1,3)`, which repeats the constant across the generated rows — is
refused rather than half-supported. It needs the other columns carried into each
generated row, and a shape that silently dropped one would be worse than an
honest refusal.

Multiranges can also be sent as bound parameters in the binary format now. The
layout is a count of ranges followed by each one in the *range's* own binary
form, so it reuses the range decoder rather than restating the flags-and-bounds
layout a second time and risking the two drifting apart.

#### Added

- `generate_series` in the select list of a `FROM`-less query, with aliases,
  `ORDER BY`, `LIMIT` and `OFFSET`.
- Multirange bound parameters in the binary wire format.
