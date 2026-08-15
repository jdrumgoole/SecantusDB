### BEFORE INSERT row triggers, with plpgsql NEW records

`CREATE TRIGGER … BEFORE INSERT ON t FOR EACH ROW EXECUTE PROCEDURE fn()`
now works end-to-end: a plpgsql `RETURNS trigger` function receives the
row as its `NEW` record, may read fields (`new.t`), assign them
(`new.ts := to_tsvector(new.t)`), and `RETURN NEW` — or `RETURN NULL` to
skip the row, exactly PG's BEFORE-trigger semantics. Triggers fire on
every insert path (INSERT and COPY FROM), die with their table, and
`pg_temp.`-qualified trigger functions resolve to the session's private
namespace. This is the tsvector-maintenance shape pgx's COPY test
exercises — the last stable failure in the pgconn package.

Every other trigger shape — AFTER, UPDATE/DELETE events,
statement-level — stays faithfully rejected rather than
stored-and-never-fired. In support: `RETURNS trigger` parses (sqlglot
rejects the bare pseudo-type; the planner quotes it pre-parse), and
`to_tsvector` now refuses words longer than 2046 characters like real
PG, so a 10 kB token yields an empty tsvector instead of a giant lexeme.

#### Added

- `sql/engine.py` / `sql/catalog.py`: CREATE TRIGGER (BEFORE INSERT ROW)
  with catalog storage, function validation (42P17 for non-trigger
  functions, 42710 duplicates), and trigger-drops-with-table.
- `sql/plpgsql.py`: record-field assignment (`new.f := …`), qualified
  record reads (`new.f`), and `invoke_trigger` with PG's
  NEW/NULL-return semantics.
- `sql/executor.py`: BEFORE INSERT row triggers fire over every planned
  row in the shared insert path (INSERT and COPY).

#### Fixed

- `sql/fts.py`: lexemes longer than 2046 characters are not indexed,
  like real PG.
