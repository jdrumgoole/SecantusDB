### The Rust PostgreSQL server learns arrays

Arrays were the largest hole left in the Rust PostgreSQL server's query
language. Storing and returning one already worked — `int[]` columns,
`ARRAY[...]` literals, multidimensional values, `||` — but almost nothing could
be *done* with one: fifteen array functions, the three containment operators,
and subscripting itself all answered `0A000 … is not supported yet`, and
`UPDATE t SET a[2] = 99` answered `cannot cast int4 to int4[]`, an error about a
cast the statement never asked for.

All of that now works. The array corpus this release was measured against went
from 24 divergences out of 29 to 6, and a second corpus of 32 further shapes
runs clean; every one of the six remaining is an honest, named refusal rather
than a wrong answer, and five of them are one missing feature (set-returning
functions — `unnest`, `generate_subscripts`, a function in `FROM`).

Two things are deliberately refused rather than approximated. This server does
not model array *lower bounds* — every array starts at subscript 1, which is
what every array a client can build actually does — so `array_fill(v, dims,
lbounds)` with a bound other than 1, and `SET a[i] = v` below subscript 1, both
answer `0A000` by name. PostgreSQL answers the second by *moving* the array's
lower bound, leaving an `[0:5]={…}`; a server that silently re-based it to 1
would hand back the right value under subscripts that then lie about it, which
is the kind of quiet divergence this project treats as data loss.

#### Added

- The array functions: `array_length`, `array_ndims`, `array_dims`,
  `array_lower`, `array_upper`, `cardinality`, `array_cat`, `array_append`,
  `array_prepend`, `array_to_string`, `string_to_array`, `array_position`,
  `array_positions`, `array_remove`, `array_replace` and `array_fill`, each
  measured against PostgreSQL 14.13 including its NULL rules — which are not
  one rule: `array_cat(NULL, ARRAY[3])` is `{3}` while `array_remove(NULL, 1)`
  is NULL, and `array_position(ARRAY[1,NULL], NULL)` finds the NULL where
  `ARRAY[1,NULL] @> ARRAY[NULL]` is false.
- The containment operators `@>`, `<@` and `&&`, which flatten both sides, so
  `ARRAY[[1,2],[3,4]] @> ARRAY[3]` is true.
- Array subscripting: `a[1]`, `a[2:3]`, `a[2:]`, `a[:2]`, `m[1][2]`,
  `m[1:2][1:1]`, and a bound that reads the row (`a[n]`). An out-of-range
  element is NULL where an out-of-range *slice* is the empty array, and a
  subscript list shorter than the array's dimensionality selects nothing —
  `(ARRAY[[1,2],[3,4]])[1]` is NULL, not the inner row.
- Subscript assignment: `SET a[i] = v` and `SET a[lo:hi] = v`, extending the
  array with NULLs past its end, building it from a NULL column, and letting a
  second assignment in one statement see the first.

#### Fixed

- A multidimensional array reported its type as `text[]` instead of the
  element array (`int4[]`, oid 1007). A binary-format client refused the
  integer rows it was handed as `_text`.
- An array whose *first* element was NULL took its element type from that NULL
  and reported `text[]`, so a client decoded the values beside it as NULL.
- An array column was sampled as NULL at plan time, so every expression over
  one was described from a NULL: `length(ta[1])` was typed `text` and the
  integer it computed went out as the string `'1'`.
- `UPDATE` with only a subscripted assignment took the bulk write path, wrote
  an empty `$set` and still reported `UPDATE 1` — a statement that claimed
  success and changed nothing.

#### Security

- `array_fill(1, ARRAY[1000000000])` and `SET a[1000000000] = 1` each size an
  array from a number the client supplies. PostgreSQL caps an array at
  134217727 elements and says so; the cap is now checked before any
  allocation, so a single statement can no longer exhaust the server's
  memory.
