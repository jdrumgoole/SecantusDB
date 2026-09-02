### Integer columns accepted values they cannot hold

`INSERT INTO t (i) VALUES (2147483648)` into an `int` column succeeded and
stored the value. The column's declared type and its contents then disagreed,
and the row description still advertised a four-byte integer for it. `smallint`
and `bigint` behaved the same way, and `1e10::int` returned a ten-digit number
instead of failing.

PostgreSQL rejects all of these with "integer out of range", and now so does
SecantusDB — on `INSERT`, on `UPDATE`, and on an explicit cast. An expression
that overflows cannot reach a column by any of those routes. Values at the
exact boundaries are still accepted.

#### Fixed

- `smallint`, `integer` and `bigint` reject out-of-range values with
  `22003 … out of range`, instead of storing them.

#### Known limitation

Arithmetic that overflows *without* being stored — `SELECT 2147483647 + 1` —
still returns the wide result rather than failing. Storing it anywhere is
rejected.
