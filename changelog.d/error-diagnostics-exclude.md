### Constraint violations carry PG's error-identity fields, and EXCLUDE lands

Every constraint violation the SQL engine raises now attaches the
ErrorResponse identity fields real PostgreSQL sends — schema, table,
column, constraint, and datatype — which drivers surface through
psycopg's `diag` and pgjdbc's `ServerErrorMessage`. Duplicate keys name
the violated constraint (the declared PK name, not a synthesized one),
NOT NULL violations name the column, domain CHECK failures name the
domain and its check constraint, and foreign-key violations name the
referencing table. pgjdbc's ServerErrorTest — which asserts exactly
these fields for six violation kinds — passes in full.

`EXCLUDE (col WITH =, ...)` table constraints are supported in their
equality-only form: enforcement is a unique index under the hood, but a
conflict raises PostgreSQL's `23P01 exclusion_violation` with the
`<table>_<col>_excl` constraint name. Non-equality exclusion operators
(the GiST range forms) keep the honest "not supported" error.

#### Added
- ErrorResponse diagnostic fields (s/t/c/n/d) on unique, not-null,
  check, domain-check, foreign-key, and exclusion violations.
- Equality-only `EXCLUDE` table constraints, violating with 23P01.

#### Fixed
- Duplicate-key errors name the violated constraint instead of the
  table; schema-qualified tables report schema and bare name separately.
