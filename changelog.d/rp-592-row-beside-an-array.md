### Rust PG server: a row beside an array names both types

`select fs = array[1] from fs`, where `fs` is a table, failed with 42883
`operator does not exist: text = integer[]`. The SQLSTATE was right, but the
message named the row `text`. Some forms of the same comparison returned a
result instead of an error. `nullif(fs, array[1])` returned the row,
`fs || array[1]` concatenated the row into the array, and `fs in (fs,
array[1])` answered true. `fs is distinct from array[1]` and `fs between
array[1] and array[2]` gave the error with no position. PostgreSQL types an
`ARRAY[...]` from its elements when it analyses the statement. A row has no
operator against an array, so PostgreSQL 15.19 reports 42883 `operator does
not exist: fs = integer[]` at the operator. `||` accepts a row only when the
array beside it holds rows.

#### Fixed

- A row beside an array is now 42883 at the operator, and the message names
  the row's own type (`fs`, `ct`, `record`). The array is typed as
  PostgreSQL types it:
  - an `ARRAY[...]` takes its elements' common type, so `array[1, 2.5]` is
    `numeric[]`;
  - an array of untyped literals or NULLs alone is `text[]`;
  - a nested `ARRAY[ARRAY[1]]` is `integer[]`;
  - a cast to an array type (`'{1}'::int[]`, `array[1]::bigint[]`) is that
    type.

  This holds for every comparison, IS [NOT] DISTINCT FROM, NULLIF, each IN
  item and BETWEEN. `fs <> all (array[array[1]])` compares the row with the
  nested array's element type (`fs <> integer`).
- `||` between a row and an array is now 42883 (`fs || integer[]`) unless
  the array's elements are rows: `fs || array[fs]` still concatenates.

Fixes RP-592 (tests/backlog/test_rp_found_rp530.py::test_row_beside_an_array_constructor)
