### `_pg_expandarray` and composite field access in the select list

`information_schema._pg_expandarray(arr)` yields one `(x, n)` record per array
element — the value and its 1-based subscript. JDBC's metadata queries lean on
it heavily, selecting it two ways in the same statement: the whole record, and
a single field via `(…).n`. Neither shape was recognised, so those queries
failed outright rather than returning primary-key or index information.

Both now work, including the schema-qualified spelling, and the record stays a
composite rather than being flattened to text so that a field can still be read
from it a level up — which is exactly how the driver uses it, producing the
record in a subquery and selecting a field from it in the outer query.

#### Added

- `information_schema._pg_expandarray` in the select list, whole or by field.
- `(expr).field` against a record-returning function.

#### Fixed

- Set-returning functions are recognised when written with a schema
  qualification in the select list.
