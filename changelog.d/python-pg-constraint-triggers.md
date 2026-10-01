### Python PostgreSQL server: constraint triggers and INSTEAD OF triggers

`CREATE CONSTRAINT TRIGGER` now runs on the Python PG server. Each behaviour below was checked against PostgreSQL 15.

#### Added

- A `DEFERRABLE INITIALLY DEFERRED` constraint trigger queues its row events for COMMIT. If the trigger raises there, the whole block rolls back.
- `SET CONSTRAINTS name | ALL IMMEDIATE` runs the queued events and switches the trigger to fire at once. Setting a non-deferrable constraint trigger answers `42809`.
- Outside a transaction block, a deferred constraint trigger runs at the end of the statement, after the immediate triggers.
- `CREATE CONSTRAINT TRIGGER` rejects a BEFORE trigger, a statement-level trigger, and `NOT DEFERRABLE INITIALLY DEFERRED` with PostgreSQL's `42601`.
- `INSTEAD OF` row triggers on views. Each row an `INSERT`, `UPDATE` or `DELETE` names goes to the trigger instead of the base table. A NULL return skips the row, the command tag counts the rest, and `RETURNING` projects what the trigger returned.
- `CREATE TRIGGER` rejects the shapes PostgreSQL rejects for views and tables, with the same SQLSTATE, message and detail. Dropping a view drops its triggers.
