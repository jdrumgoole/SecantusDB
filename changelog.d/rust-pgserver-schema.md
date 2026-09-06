### CREATE SCHEMA

`CREATE SCHEMA` and `DROP SCHEMA` are accepted now, tracked in the same
`__sql_schemas__` catalog the Python server uses so the two agree on which
schemas exist. Schema-qualified names already resolved by their last part —
`s2.t` finds table `t` — so a table or type living in a schema works the moment
the schema DDL is allowed.

A duplicate is `42P06` (distinct from a table's `42P07` and a type's `42710`),
a missing `DROP SCHEMA` is `3F000`, `IF NOT EXISTS` / `IF EXISTS` are no-ops on
the wrong-existence case, and `CASCADE` is accepted (this server does not track
which objects belong to a schema — names carry none — so it drops the schema
record; the objects are dropped by name elsewhere).

#### Added

- `CREATE SCHEMA [IF NOT EXISTS]`, `DROP SCHEMA [IF EXISTS] … [CASCADE]`, with
  PostgreSQL's error classes.
