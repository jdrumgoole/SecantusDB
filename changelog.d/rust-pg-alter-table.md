### ALTER TABLE on the Rust PostgreSQL server

`ALTER TABLE` did not exist in any form — `AlterTableStmt is not supported
yet` — which put every migration tool out of reach. `ALTER TABLE ... RENAME`
was a separate refusal (`RenameStmt`), because PostgreSQL's parser puts it in
a different node.

Both work now. Against a live PostgreSQL 14.13 the DDL corpus went from 28
divergences out of 41 to 7, and every one of the seven left is `CREATE INDEX`
or `CREATE VIEW` — a different slice. A second corpus of 85 lines written for
this change, covering the shapes the first one never reached, is clean.

What works: `ADD COLUMN` (with `DEFAULT`, `NOT NULL`, `IF NOT EXISTS`),
`DROP COLUMN`, `ALTER COLUMN SET`/`DROP DEFAULT`, `SET`/`DROP NOT NULL`,
`ALTER COLUMN TYPE`, `ADD CONSTRAINT ... CHECK`, `DROP CONSTRAINT`,
`RENAME COLUMN` and `RENAME TO` — several actions in one statement, applied in
order, so `add column m int, alter column m set default 5` works.

Three decisions worth stating, because each is a place where a plausible
implementation is silently wrong:

**The rows are rewritten, not left short a field.** `ADD COLUMN` fills every
existing row and `DROP COLUMN` removes the field. Leaving the field behind
would be invisible while the catalog no longer named it — and then adding a
column of the same name later would resurrect the old values, a wrong answer
no error would flag. It makes an ALTER O(table) where PostgreSQL can often
avoid the rewrite; that is the right trade here, where tables are fixtures.

**`ALTER COLUMN TYPE` follows PostgreSQL's cast rule, which is about the TYPES
and not the values.** `text -> int` is refused with `42804` even when every
value would convert cleanly; it needs a `USING` clause. Casting per row
instead made the same statement succeed or fail depending on the data, which
is not what PostgreSQL does either way. The rule was measured across 31 type
pairs on 14.24.

**An ALTER is validated before a row is touched.** Every action is checked
against the catalog first, so a second action failing cannot leave the table
in a shape neither the old nor the new catalog describes. `SET NOT NULL` over
a column that has NULLs, and `ADD CONSTRAINT CHECK` that existing rows fail,
are both refused with PostgreSQL's own SQLSTATE.

#### Added

- `secantus-pgplan` / `secantus-pgserver`: `ALTER TABLE` and
  `ALTER TABLE ... RENAME`, in the forms above. An action outside them is
  refused by its own name (`ALTER TABLE OWNER TO`, `... VALIDATE CONSTRAINT`)
  rather than under one catch-all.
- Savepoint capture for the new statements, so a `ROLLBACK TO` puts back the
  rewritten ROWS as well as the catalog.

#### Fixed

- `secantus-pgplan`: `CREATE TABLE t (id int, id int)` was accepted silently,
  producing a table whose second column was unreachable because every lookup
  resolves a name to the first match. It is PostgreSQL's `42701` now.
