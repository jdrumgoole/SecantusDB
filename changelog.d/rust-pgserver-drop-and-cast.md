### DROP TABLE and type casts on the Rust PostgreSQL server

`DROP TABLE`, `DROP TABLE IF EXISTS` and casts (`'1'::int`, `1::text`,
`$1::float8`) now work. Both came straight off the ranked list that psycopg's
test suite produced when it was first pointed at the Rust server — measuring
against someone else's tests turns out to be a much better guide to what to
build next than deciding for oneself.

Casts brought a subtler problem than converting values. A client asks what
columns a query returns *before* it supplies any parameter values, so a
column's type cannot be read off the value it happens to hold — at that point
`$1::int` has no value at all. Inferring from the value typed that column as
text, and the client then decoded a perfectly good integer as a string. Types
now come from the cast that declares them. The same fix corrected `text`
columns, which were being reported as `varchar`: PostgreSQL treats those as
different types, and while Python clients decode both to strings, the Java and
Go drivers do not.

The gauge moved from 694 to 746 of psycopg's 4,238 tests. The modest jump is
itself informative: a test that was blocked by a missing `DROP TABLE` usually
needs several other things too, so removing one obstacle mostly reveals the
next. Expressions in a `SELECT` list are now the single largest blocker.

#### Added

- `DROP TABLE`, including `IF EXISTS` and multiple tables in one statement.
- Casts to the integer, floating-point, boolean and text types, with
  PostgreSQL's `invalid input syntax` error for values that cannot convert.

#### Fixed

- `text` columns were reported over the wire as `varchar`.
