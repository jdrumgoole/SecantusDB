### Comparing a text column against an integer now errors, like Postgres

Postgres resolves comparison operators while it analyses a statement, not while
it reads rows, so `SELECT * FROM t WHERE text_col = 42` never returns an empty
result — it fails with `operator does not exist: text = integer` before the
first row is touched. SecantusDB's SQL layer evaluated comparisons with Python's
`==` on decoded BSON values, which absorbed the mismatch and quietly answered
"no rows". A query with a genuine type bug in it looked like a query with no
matching data.

The Postgres front end now performs that operator resolution at plan time. A
comparison whose two operands are both confidently typed, and whose types fall
in two different categories — numeric, string, boolean, date-time — raises
`42883` with Postgres' message and no rows read. Everything the analysis cannot
decide with certainty is left exactly as it was: an untyped string literal still
takes the other operand's type (`text_col = '42'` and `int_col = '42'` both keep
working), and bound parameters, subqueries, arithmetic, unrecognised functions,
CTEs, derived tables and views are all passed through untouched. A false
positive here would break a working query, so the rule is sound rather than
complete.

Schema-on-read tables are exempt on purpose. A reflected collection's column
types are inferred by sampling fifty documents, so a heterogeneous BSON field
can be declared `text` while holding integers — comparing across BSON types is
the whole point of the dual-protocol path, and it stays lenient there.

#### Added

- `sql/typecheck.py`: plan-time comparison-operator resolution, raising
  `42883 undefined_function` for a cross-category comparison on declared
  tables. Reflected tables, unmodelled type categories (`bytea`, `uuid`,
  `json`, `money`, `interval`, `time`, arrays, ranges, geo, network, bit),
  untyped literals, parameters, and subquery scopes are all left lenient.
