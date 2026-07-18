### $sample validates its size argument, like mongod

The Python server's `$sample` stage coerced its `size` with a naive `int()`, so a
bool size was treated as 1 and a negative size crashed with a raw `ValueError`.
mongod rejects both — a non-number size with 28746 ("size argument to $sample
must be a number") and a negative size with 28747 ("must not be negative") — while
accepting a fractional double and truncating it. The Python server now matches;
the Rust server already rejected these (its `$sample` lives in the server crate and
validated), so this closes the gap on the Python side.

With this, the aggregation numeric-argument trio — `$limit`, `$skip`, and
`$sample` — matches mongod on both servers.

#### Fixed

- `$sample` rejects a bool `size` (28746) and a negative `size` (28747) instead of
  coercing the bool to 1 or crashing on the negative (Python server).
