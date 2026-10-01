### Python PostgreSQL server: trigger transition tables, and three `string_agg` fixes

AFTER triggers can now declare `REFERENCING OLD TABLE AS ... NEW TABLE AS ...` and query the statement's affected rows as a relation. Each change was checked against PostgreSQL 15.

#### Added

- Transition tables on every write path: INSERT, UPDATE, DELETE, ON CONFLICT, UPDATE FROM, DELETE USING and MERGE. They are available to both statement-level and row-level AFTER triggers.
- `CREATE TRIGGER` rejects the shapes PostgreSQL rejects, with the same SQLSTATE and message: a BEFORE trigger, more than one event, a column list, the wrong OLD/NEW side for the event, and TRUNCATE.

#### Fixed

- A `string_agg` nested in an expression no longer drops its in-call `ORDER BY`. For example, `coalesce(string_agg(v, ',' ORDER BY id DESC), '')` came back in insertion order with no error.
- A `string_agg` nested in an expression over a JOIN no longer fails with `0A000 unsupported aggregate`.
- `||` with a non-text operand inside an aggregate (`string_agg(id || '=' || v, ',')`) now casts the operand to text. It used to fail with `$concat only supports strings`.
