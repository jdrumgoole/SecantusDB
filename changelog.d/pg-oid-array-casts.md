### `oid[]` casts parse, and `regclass` columns no longer crash

On the Python PostgreSQL server, a cast to `oid[]`, `regclass[]`, `regproc[]`
or `regtype[]` (`'{1,2}'::oid[]`) was rejected as a syntax error, for SQL
PostgreSQL accepts. psycopg writes an `oid[]` value exactly that way when it
fills parameters in on the client side, so inserting such a value through a
client-side cursor failed. These casts now work, and a `regtype[]` column can be
created.

Creating a table with a `regclass` or `regproc` column failed with an internal
error. The server does not support those column types; it now says so with a
"not supported" error instead of crashing.

Found by replaying psycopg's own random-data tests through the parser: 47 of
3,000 random schemas hit it before the fix, none after.

#### Fixed

- `sql/planner.py`: `oid[]` / `regclass[]` / `regproc[]` / `regtype[]` in a type
  position are rewritten to a form the SQL parser accepts and restored after
  parsing, using the parser's own tokens so string literals are never touched.
- `sql/planner.py`: column-type checks no longer assume every parsed type is an
  enum member (`regclass` is kept as a plain string).

#### Testing

- `tests/test_pg_oid_array_casts.py`: every cast and column shape, a string
  literal left alone, the array type reported back, a client-side-bound `oid`
  list, and `regclass` / `regproc` columns failing cleanly. Fifteen of the
  sixteen tests fail without the fix.
