### SQL writes can no longer read tables they weren't granted

The SQL server's per-statement RBAC authorized only a write statement's
primary target table — the table a subquery, `FROM`, `USING`, or
`AS SELECT` clause *read from* was never checked. A principal holding
nothing but an `INSERT` grant on one table could run
`INSERT INTO granted SELECT * FROM secret RETURNING *` and receive
`secret`'s rows in the response, defeating the finer-grained
table/column grant model the SQL layer exists to provide.

Every table a write statement reads as a source now requires its own
`find` (SELECT) grant — db-wide role or table-level `GRANT SELECT` —
across `INSERT ... SELECT`, `UPDATE ... FROM`, `DELETE ... USING`,
`CREATE TABLE ... AS SELECT`, and subqueries. CTE names are excluded
(query-local, not base tables) and a self-referential
`INSERT INTO a SELECT ... FROM a` is not charged an extra read grant
(the actor already writes `a`, so no *other* table leaks). Plain
`SELECT`s are unaffected — their reads, including column-level grants,
are authorized exactly as before.

#### Security

- `INSERT ... SELECT`, `UPDATE ... FROM`, `DELETE ... USING`, and
  subqueries checked RBAC only on the primary write target, so a
  write-only grant leaked an unrelated table's rows through the source
  clause (#785).
- `CREATE TABLE ... AS SELECT` authorized only `CREATE` on the new
  table, never a read grant on its `SELECT` source, so a create-capable
  role could copy out any table (#881).
