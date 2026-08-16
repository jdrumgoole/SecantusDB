### COMMENT ON FUNCTION and table row types as column types

`COMMENT ON FUNCTION f(args)` (and the bare-name form) stores the
comment on the function's catalog entry, and a column may now be
declared with a table's name as its type — in PostgreSQL every table is
also a composite row type, so `CREATE TABLE t (col other_table)` stores
that row shape and supports `(col).field` access. These were the two
setup blockers behind four pgjdbc DatabaseMetaData/ResultSetMetaData/
RefCursor test classes whose entire suites died in class setup;
ResultSetMetaDataTest alone now runs its 60-test matrix.

#### Added
- `COMMENT ON FUNCTION` (parenthesised and bare-name forms; 42883 for
  unknown functions, 42725 for ambiguous overloads).
- Columns typed by a table's row type.
