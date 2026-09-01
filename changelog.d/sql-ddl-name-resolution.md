### `ALTER TABLE` works outside `public`, and DDL errors name the right relation

`ALTER TABLE` never looked at the schema. It read the bare relation name and
threw away the qualifier — whether that qualifier came from `search_path` or
was written out as `schema.table` — so **every** form of the statement
(`ADD COLUMN`, `DROP COLUMN`, `RENAME COLUMN`, `RENAME TO`, `ALTER COLUMN`,
`ADD CONSTRAINT`, `ADD PRIMARY KEY`) answered `relation "…" does not exist`
for any table outside `public`. `TRUNCATE`, `CREATE INDEX`, `DROP TABLE` and
`COMMENT ON` had always resolved correctly, which is what kept it hidden.

Behind that sat a second defect: because the rename target was also read bare,
a successful `ALTER TABLE s.a RENAME TO b` would have written the table back as
`public.b` — moving it out of its schema.

The same probe run turned up three more, all found by running each shape
against PostgreSQL 14.13 rather than reading the code. `DROP SCHEMA … CASCADE`
dropped a schema's tables and types but left its views, materialized views and
sequences behind, and a bare `DROP SCHEMA` did not count them as dependants —
so they outlived the schema and then collided with a later `CREATE`. A bare
`CREATE SEQUENCE` ignored `search_path` and always created in `public`, so a
following `CREATE SEQUENCE schema.s` saw a free name and quietly made a second
sequence where PostgreSQL raises `42P07`. And an error naming a relation
reported the resolved catalog key instead of what the statement said, so a
missing `onlypub` under `SET search_path TO sa` came back as
`relation "sa.onlypub" does not exist` — naming a schema the user never typed.

#### Fixed

- `ALTER TABLE` resolves its target through `search_path` and honours an
  explicit `schema.table` qualifier, across all seven statement forms.
- `ALTER TABLE … RENAME TO` keeps the relation in its own schema, rejects a
  schema-qualified new name with `42601` (as PostgreSQL does), and answers
  `42P07` when the new name is already taken — including a rename to the
  relation's current name.
- `DROP SCHEMA … CASCADE` drops the schema's views, materialized views and
  sequences; without `CASCADE` those objects now raise `2BP01` like any other
  dependant.
- A bare `CREATE SEQUENCE` is created in the first schema on `search_path`.
- A "does not exist" error names the relation as the statement wrote it, and
  keeps an explicit `public.` qualifier.
- `DROP TABLE` on a missing relation says `table "x" does not exist` rather
  than `relation "x"`, matching the noun PostgreSQL uses for each `DROP` verb.
- A `DROP` naming a relation that exists under a different kind answers
  `42809 "x" is not a table` instead of claiming the object is absent — which
  had made `DROP TABLE IF EXISTS <a view>` succeed silently while the view
  survived.
- `42P07 relation "…" already exists` names the bare relation, as PostgreSQL
  does, instead of the schema-qualified catalog key.
- Materialized views are schema-aware. Every matview path read the bare name,
  so a matview was created, catalogued and stored unqualified whatever schema
  the statement gave — `SELECT … FROM schema.mv` raised `42P01`, and two
  schemas could not hold same-named matviews. `CREATE` / `DROP` / `REFRESH` /
  `ALTER … RENAME TO` and the not-populated check all resolve properly now, and
  `DROP VIEW` and `DROP MATERIALIZED VIEW` no longer reach each other's
  relations.
- `SELECT … INTO t` works. PostgreSQL's older spelling of `CREATE TABLE t AS
  SELECT …` was not dispatched at all: the target was resolved as if it were a
  source table, so every such statement failed with `relation "t" does not
  exist`.
- A sequence can be read as a relation — `SELECT last_value, is_called FROM s`
  — as it can in PostgreSQL. `last_value` reports the value actually handed
  out; reading the stored counter would have shown the pre-allocated batch's
  high-water mark, which runs ahead of the sequence's real position. (`log_cnt`
  is reported as 0: it counts values PostgreSQL has pre-logged to WAL and has
  no counterpart here.)

#### Changed

- `CREATE TABLE AS`, `SELECT INTO` and `CREATE MATERIALIZED VIEW` now report
  the number of rows they wrote as the driver's row count, and send no row
  description. They carry a `SELECT n` command tag, and the wire layer took
  that tag alone as meaning "rows follow" — so each sent an empty row
  description and clients read the row count as 0.
