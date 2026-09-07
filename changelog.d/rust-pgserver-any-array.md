### Rust pgserver: `ANY` / `ALL` array operators

`scalar <op> ANY(array)` and `scalar <op> ALL(array)` work now — the form
psycopg renders an `IN`-list into (`col = ANY(%s)`), so a whole family of cursor
and array tests turned on it. All six comparison operators are supported, in
both a `SELECT` expression and a `WHERE` clause, with PostgreSQL's exact
three-valued logic: `ANY` is true on the first match, `ALL` false on the first
mismatch, a NULL element or NULL scalar yields NULL, an empty array is false for
`ANY` and true for `ALL`. An untyped array parameter (which arrives as array
literal text) is coerced to the column's element type, matching how PostgreSQL
resolves an unknown `ANY` operand. A scalar compared to an array with no
`ANY`/`ALL` is `42883` (no such operator), as on PostgreSQL — an array-to-array
comparison is unaffected.

#### Added
- `= / <> / < / <= / > / >=` with `ANY(array)` and `ALL(array)`, in `SELECT`
  and `WHERE`, over array literals and array parameters (including untyped ones).

#### Fixed
- A scalar compared to an array without `ANY`/`ALL` now raises `42883` instead
  of silently matching nothing (WHERE) or reporting a wrong operand type.
