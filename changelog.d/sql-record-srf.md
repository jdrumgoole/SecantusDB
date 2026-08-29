### `ORDER BY` works over `_pg_expandarray`, and its value column has a type

Selecting the value field of a record set-returning function —
`(information_schema._pg_expandarray(arr)).x`, the shape JDBC's metadata queries
use — had two problems.

Ordering the result did not work. `ORDER BY <alias>` and ordering by the
subscript field both failed outright with "column does not exist", because the
name only exists in the expanded output, not in the source row. `ORDER BY 1` was
worse: it was accepted and then ignored, so the query reported success and
returned the array's own order. All of these now sort, matching PostgreSQL.

The value column also reported no type at all rather than the array's element
type, so a client asking what it had just selected got nothing usable. It now
reports `int4` for an integer array, `text` for a text array, `numeric` for a
numeric one — the subscript field was always correct.

One test that pinned the old ordering behaviour has been rewritten. It was
named so it could not be mistaken for intended behaviour, and it failed as soon
as the behaviour improved, which is what it was there for.
