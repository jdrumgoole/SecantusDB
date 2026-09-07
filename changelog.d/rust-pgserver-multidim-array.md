### Rust pgserver: multidimensional arrays

Multidimensional arrays now cross the wire. They already stored and text-
rendered, but returning a nested array to a client was refused — so
`SELECT ARRAY[[1,2],[3,4]]`, reading a 2-D `int[]` column, and casting a nested
text literal all failed. The server now builds PostgreSQL's multidimensional
array binary wire form by hand (an `ndims` header, per-dimension bounds, and
the leaves in row-major order) and also emits the `{{1,2},{3,4}}` text form, so
a column reaches the client correctly whether the cursor asked for text (the
psycopg default) or binary. A nested `ARRAY[...]` constructor now reports the
array type (oid 1007), not `varchar`; the array cast recurses through the
nesting; and a ragged array is rejected exactly as PostgreSQL rejects it —
`2202E` from a constructor, `22P02` from a text literal.

#### Added
- Multidimensional array results (2-D, 3-D, …) in both the text and binary wire
  formats, for `int` / `bigint` / `smallint` / `float` / `numeric` / `bool` /
  `text` element types.
- A nested array constructor is typed as its array type (oid 1007), and the
  array cast handles nested elements.

#### Fixed
- A ragged multidimensional array is now rejected (`2202E` / `22P02`) instead of
  being silently accepted.
