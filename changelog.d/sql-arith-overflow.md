### Arithmetic and casts that overflow now say so

Python's `int` is unbounded and its `float` saturates to infinity, so a result
that no Postgres type can hold was computed **silently**. `i + 1` on an `int`
column answered 2147483648 — and sent it under oid 23, four bytes that cannot
carry it — where PostgreSQL answers `22003 integer out of range`.

The width has to come from the declared operand types, not from the value:
`s + 1` on a `smallint` is `int4` arithmetic in PostgreSQL, so 32768 is a
correct answer there, while `32767::smallint * 2::smallint` overflows. The two
are indistinguishable by value alone. The planner already implements
PostgreSQL's promotion table for the RowDescription, so it now stamps that
width on each arithmetic node and the evaluator checks against it.

`1e39::float4` was worse than wrong: `struct.pack('!f', …)` raised
`OverflowError` and reached the wire as an `XX000` internal error.

#### Fixed

- `22003` for integer overflow at `int2` / `int4` / `int8` — through `+`, `-`,
  `*`, `/`, `abs()` and unary minus, and in every clause that reaches the
  scalar evaluator: projections, `INSERT`, `UPDATE`, subqueries, CTEs,
  `GROUP BY`, `HAVING` and `ORDER BY`, with or without a `FROM`.
- `22003` for `float8` overflow and underflow, following PostgreSQL's
  `CHECKFLOATVAL` rule — an infinite result is an error unless an operand was
  already infinite, and a zero result is an error unless zero was a legal
  answer.
- `22003` instead of `XX000` for a float cast out of range, in both spellings
  PostgreSQL uses: casting from numeric or text quotes the input, while
  narrowing an existing double reports `value out of range: overflow`.

#### Still divergent

Arithmetic inside a `WHERE` clause. It lowers to a Mongo `$expr` evaluated by
the operator engine the MongoDB server shares, and teaching that engine
PostgreSQL's integer widths would break the layer boundary.
