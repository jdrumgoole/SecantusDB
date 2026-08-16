### DatabaseMetaDataTest setup unblocked: pg_description DML, operator DDL, PK USING INDEX

`pg_description` now carries COMMENT ON FUNCTION rows (classoid
`pg_proc`), `'name'::regproc` resolves a user function to its pg_proc
oid (still rendering as the bare name), and UPDATE / DELETE statements
targeting `pg_description` work — persisted as a delta over the derived
comment rows. Real Postgres lets a superuser edit the catalog directly;
pgjdbc's DatabaseMetaDataTest setup does exactly that (moving a function
comment onto a table's oid to prove the metadata queries' classoid
guards), and the whole ~90-test class aborted at setup without it. The
same setup also needed `CREATE OPERATOR` / `DROP OPERATOR` (registered
DDL; expression evaluation doesn't consult user operators) and
`ALTER TABLE … ADD PRIMARY KEY USING INDEX` (promotes an existing
unique index to the primary key, taking the index's name).

#### Added
- Function-comment rows in `pg_description` (objoid = pg_proc oid,
  classoid 1255).
- `'name'::regproc` resolves unique user functions to their minted oid,
  comparing equal to both the oid and the name.
- UPDATE / DELETE against `pg_description` (suppress + re-emit delta,
  persisted per database, savepoint-aware via the catalog snapshot set).
- `CREATE OPERATOR name (LEFTARG = …, RIGHTARG = …, PROCEDURE = …)` and
  `DROP OPERATOR [IF EXISTS] name (left, right)`.
- `ALTER TABLE … ADD PRIMARY KEY USING INDEX idx` — validates the index
  is unique, re-keys rows, reflects the PK constraint under the index's
  name in `pg_constraint`.
- Array-of-composite columns (`custom[]`) — stored as subdocument lists,
  reporting the composite's minted array-companion oid.

#### Fixed
- OUT-only parameters no longer count toward a function's signature:
  `f3(IN a int, INOUT b varchar, OUT c timestamptz)` is `f3(int,
  varchar)` to DROP FUNCTION and callers, matching PG's identity rule.
