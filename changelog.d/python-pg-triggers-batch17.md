### The Python PostgreSQL server runs AFTER, statement-level, UPDATE and DELETE triggers

Until now the Python PostgreSQL server ran only `BEFORE INSERT FOR EACH ROW`
triggers. It now runs the trigger kinds most applications use, as PostgreSQL
does, and refuses with `0A000` the ones it still cannot run.

#### Added

- `CREATE TRIGGER`:
  - `BEFORE` and `AFTER`, `FOR EACH ROW` and `FOR EACH STATEMENT` (the
    default), on `INSERT`, `UPDATE` and `DELETE`, several events joined with
    `OR`;
  - `WHEN (...)` conditions over `NEW` / `OLD`.
- Inside a trigger function:
  - `TG_OP`, `TG_WHEN`, `TG_LEVEL`, `TG_NAME` and `TG_TABLE_NAME`;
  - `OLD` / `NEW` are NULL where the event has none, and a field of a NULL
    record is NULL;
  - SQL statements in the body can read `new.x` / `old.x`, so an audit
    trigger can insert them into another table.
- Firing:
  - a BEFORE ROW trigger can change `NEW`, or skip the row with `NULL`;
  - statement-level triggers fire even when no row matches.
- Triggers the Rust server stores in the shared catalog fire here too.

#### Fixed

- **A write whose trigger wrote, then failed, left the trigger's writes
  committed.** A single INSERT / UPDATE / DELETE / MERGE now runs as one
  transaction whenever the database has a trigger, as on PostgreSQL.

Still refused, with `0A000`:

- `UPDATE OF` column lists, trigger arguments, transition tables
  (`REFERENCING`), constraint triggers, `INSTEAD OF` and `TRUNCATE`;
- a trigger on a path that fires none: `ON CONFLICT`, `MERGE`,
  `UPDATE ... FROM` and `DELETE ... USING`.

The firing order and logged values are checked against PostgreSQL 15's output
for the same statements.
