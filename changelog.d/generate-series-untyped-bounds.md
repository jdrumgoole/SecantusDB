### `generate_series` accepts an untyped parameter as a bound

`select generate_series(1, $1)` — with the parameter sent without a type OID,
as clients routinely do — was rejected outright. Nothing upstream had coerced
the value, because the wire never said what type it was, so the bound arrived
as text and the function refused it as a non-numeric range. Real PostgreSQL
infers the parameter's type from the argument position and reads it as an
integer.

The cost of this was out of all proportion to the gap. The pgx driver's test
suite calls that exact query in a helper that runs at the end of 66 of its
connection tests, to check the connection is still usable. Every one of those
tests failed at the final step, regardless of what it was actually testing —
which made a single missing coercion look like sixty-six unrelated bugs.

#### Fixed

- A numeric-looking text bound (or step) is parsed as a number, so
  `generate_series` works with untyped parameters. Bounds that are genuinely
  not numbers still raise, and `numeric` bounds are now accepted alongside
  int and float. The pgx `pgconn` package goes from 86 failures to 29.
