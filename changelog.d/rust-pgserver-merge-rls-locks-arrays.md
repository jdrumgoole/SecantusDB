### The Rust PostgreSQL server: MERGE, row-level security, table locks and timeouts, and autocommit writes that no longer lose updates

An autocommit `UPDATE ... SET n = n + 1` now runs in a transaction of its
own. Before, a row changed by another session between this statement's read
and its write was silently overwritten: the committed value was lost, where
PostgreSQL waits for the other writer and builds on its result. A write that
collides with another transaction's now waits, and re-evaluates against the
row that transaction left, as READ COMMITTED does. The wait honours a cancel,
`statement_timeout` and `lock_timeout`.

A primary key column accepted NULL, and did so repeatedly. It is now NOT
NULL, as in PostgreSQL. A table rewrite (`ALTER COLUMN TYPE`, `CLUSTER`) and
an autocommit `COPY FROM` are now atomic. A duplicate key midway through
either used to leave the table half-written.

The server reports PostgreSQL 15.0, and now has what 15 added: `MERGE`,
`regexp_count` / `regexp_instr` / `regexp_substr` / `regexp_like`, and
`UNIQUE NULLS NOT DISTINCT`. SQL/JSON syntax from 16 and 17 is answered as
15 answers it. The differential probe can run a corpus against a PostgreSQL
15 reference (`# reference-version: 15`).

#### Added

- `MERGE INTO ... USING ... ON ... WHEN [NOT] MATCHED [AND ...] THEN UPDATE |
  DELETE | INSERT | DO NOTHING`, with PostgreSQL 15's `21000` for a target
  row matched twice.
- Row-level security enforced: permissive and restrictive policies per
  command, `WITH CHECK` on new rows, `FORCE`, `BYPASSRLS`, and the SELECT
  policies on an UPDATE or DELETE that reads its rows.
- Table privileges read off the SQL as written. A view is checked as the
  view, and its base tables as the view's owner. A subquery anywhere in the
  statement, correlated or not, is checked with it.
- Arrays whose lower bound is not 1 (`'[0:1]={a,b}'`, `array_fill(v, dims,
  lbounds)`, an assignment below or past the bounds). They keep their bounds
  through text and binary I/O, subscripts, `array_lower` / `array_dims`,
  equality, and the functions that carry them.
- Timeouts and table locks:
  - `statement_timeout` (57014) and `lock_timeout` (55P03).
  - `LOCK TABLE ... IN <mode> MODE [NOWAIT]`, with PostgreSQL's conflict
    table.
- Maintenance statements:
  - `CLUSTER`, which rewrites a table in an index's order.
  - `VACUUM`, `ANALYZE`, `CHECKPOINT` and `REINDEX`, with PostgreSQL's
    validation.
- `ALTER TABLE` forms:
  - `ADD UNIQUE`, `ADD PRIMARY KEY` (the rows are re-keyed) and `ADD FOREIGN
    KEY`, each checked against the existing rows.
  - `ADD COLUMN` with inline constraints.
  - `ALTER COLUMN TYPE ... USING`.
  - `ADD CHECK ... NOT VALID` / `VALIDATE CONSTRAINT`.
  - `CLUSTER ON` / `SET WITHOUT CLUSTER`.
- SQL `PREPARE` / `EXECUTE` / `DEALLOCATE`, listed in
  `pg_prepared_statements` with `from_sql`.
- `WITH ORDINALITY`, and `ROWS FROM (f(...), g(...))`.
- Several set-returning functions in one select list, run in lockstep.
- A bare `VALUES` with `ORDER BY` / `LIMIT` / `OFFSET`.
- `(a, b) IN (SELECT ...)` and `NOT IN`.
- New functions and aggregates:
  - `range_agg` and `range_intersect_agg`.
  - `trim_array`.
  - `IS [form] NORMALIZED`.
- Triggers:
  - `INSTEAD OF` triggers on views.
  - Constraint triggers and `SET CONSTRAINTS`.
  - Transition tables (`REFERENCING OLD/NEW TABLE`).
- User-defined functions:
  - Overloads.
  - `VARIADIC`.
  - SQL-standard bodies.
- Types:
  - The geometric types.
  - `regnamespace`, `regrole`, `regproc` and `regprocedure`.
- `pgcrypto` and jsonpath `.datetime()`.
- Roles:
  - Role membership: `IN ROLE`, `GRANT role`, `pg_has_role`,
    `pg_auth_members`.
  - md5 passwords and `VALID UNTIL`.

#### Fixed

- A parameter compared with a column (`WHERE id = $1`, `SET n = $1`) is
  described with the column's type.
- `WITH ORDINALITY` and `ROWS FROM` silently dropped a column, and a bare
  `VALUES` ignored its `ORDER BY`. A FROM subquery with one output name twice
  answered the second column for both.
- An `array_cat` with an untyped literal beside an array now resolves. A
  bare boolean column works in a FILTER, and `NOT` of one too.
- POSIX character classes (`[[:alpha:]]`) follow the UTF-8 ctype, and
  `pg_database` reports the `C.UTF-8` the server behaves as. A strict jsonpath
  datetime template reports trailing or missing input.
- A storage insert's rejected rows were dropped by several callers. Every one
  now reports the first.
