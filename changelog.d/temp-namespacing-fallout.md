### Temp-namespacing fallout: bare diag names, self-referencing FKs

The psycopg gauge caught two regressions from the per-session temp-table
namespacing. Error diagnostics leaked the `pg_temp_<n>.` catalog prefix
into the TABLE NAME field, where real PG reports the bare relation name
(the schema rides in its own field). And a SELF-referencing foreign key
inside `CREATE TEMP TABLE` captured its target by the pre-rewrite bare
name — the table doesn't exist yet when references resolve — so the
constraint pointed at a nonexistent relation and never fired, including
`DEFERRABLE INITIALLY DEFERRED` checks at COMMIT.

Diagnostics now report the bare relname, and a self-referencing FK is
pointed at the table's own final (rewritten) name at plan time.

#### Fixed

- `sql/executor.py`: NOT NULL / CHECK diagnostics report the bare
  relation name; the temp schema stays in the schema field.
- `sql/planner.py`: `CREATE TABLE` re-points self-referencing FKs at the
  table's final name after the temp-namespace rewrite.
