### `unnest` over a non-integer array no longer fails the query

`SELECT unnest(ARRAY['a', 'b'])` raised `ValueError: invalid literal for int()
with base 10: 'a'` in the client. The server described the output column as
`int4` regardless of what the array actually held, then sent the text `a` — so
the driver tried to decode a string as an integer and the query died before the
application saw a row. Text, numeric and boolean arrays were all affected;
integer arrays worked, and only because the hardcoded guess happened to be
right.

The column is now described with the array's element type. Subscript-producing
functions (`generate_subscripts`, and `_pg_expandarray`'s `.n` field) stay
integers, since a position is an integer whatever the array holds.

Checked against a live PostgreSQL for text, numeric, boolean and integer arrays.

#### Fixed

- `unnest(...)` declares the array's element type instead of always `int4`, so a
  non-integer array can be unnested at all.
