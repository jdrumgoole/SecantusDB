### The Rust PostgreSQL server gains general JOINs, correlated subqueries, views and indexes

The Rust PostgreSQL server (`secantusd-pg`) used to support a JOIN only in the
narrow two-table shape psycopg's catalog queries take. It now plans any JOIN as
a source: inner, LEFT, RIGHT, FULL and CROSS joins, `USING` and `NATURAL`,
joins of three or more relations, subqueries and set-returning functions as
join sides, and every clause over them (WHERE, GROUP BY / HAVING, window
functions, DISTINCT, `SELECT *` and `t.*`). A bare column name that both sides
have is PostgreSQL's `42702` rather than the left side's value taken silently.

Correlated subqueries (`EXISTS`, `NOT EXISTS`, `IN`, `ANY` / `ALL`, `ARRAY(...)`
and scalar subqueries that read the outer row) now answer per row instead of
being refused, including in an UPDATE's SET and in an UPDATE or DELETE WHERE.
`CREATE [UNIQUE] INDEX` / `DROP INDEX` and `CREATE [OR REPLACE] VIEW` /
`DROP VIEW` are implemented, in the same on-disk shape the Python SQL server
uses, so either server reads the other's views and indexes.

#### Added

- `secantus-pgplan` / `secantus-pgserver`: general JOIN planning (`joins.rs`),
  hash-joined on the ON clause's column equalities and always decided by the
  full ON predicate.
- `secantus-pgplan` / `secantus-pgserver`: correlated subqueries, evaluated per
  outer row and memoised by the outer values (`correlated.rs`).
- `CREATE [UNIQUE] INDEX` (btree / hash, DESC, `INCLUDE`, a partial `WHERE`,
  `IF NOT EXISTS`, PostgreSQL's default `<table>_<cols>_idx` naming) and
  `DROP INDEX`, reported by `pg_indexes`. A unique index admits many NULLs, and
  a violation names the index.
- `CREATE [OR REPLACE] VIEW` (with a column list and `WITH CHECK OPTION`) and
  `DROP VIEW [CASCADE]`, with PostgreSQL's OR REPLACE column rules (`42P16`)
  and dependency errors (`2BP01`) for a view's table or view.
- `IS [NOT] DISTINCT FROM`, as a filter and as a value.

#### Fixed

- A WHERE on the nullable side of a LEFT JOIN ran before the join and kept
  the NULL-extended rows PostgreSQL drops.
- The narrow join path took a bare column name both sides have from the left
  side, and accepted a qualifier naming neither side.
- A subquery calling `nextval()` (or another volatile function) ran once per
  PLAN -- a Describe and an Execute advanced the sequence twice. It now runs
  only in the plan that executes.
- `NULL > ALL (empty set)` answered NULL; PostgreSQL answers TRUE.
- A missing qualified column is reported as `column e.nosuch does not exist`,
  as PostgreSQL words it.
