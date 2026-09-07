### Rust pgserver: ROW / record expressions

`ROW(...)` and the bare parenthesised list `(a, b, ...)` build an anonymous
record in the Rust PostgreSQL server now — reported as oid 2249, which psycopg
decodes to a Python tuple. The text form follows PostgreSQL's composite rules
(`(a,b,c)`, a NULL field empty, a field with a comma/quote/backslash/space
double-quoted, a bool printed `t`/`f`), and record comparison is exactly
PostgreSQL's three-valued logic: `=`/`<>` examine every field (a non-null
unequal field decides, else a NULL field makes the result NULL) while the
ordering operators short-circuit left to right on the first NULL or unequal
field.

#### Added
- `ROW(...)` / `(a, b, ...)` record construction (oid 2249), its `::text`
  render, and the `=`/`<>`/`<`/`<=`/`>`/`>=` record comparison operators.
