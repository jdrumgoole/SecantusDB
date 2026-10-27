### Rust PG server: a DEFERRABLE primary key is deferrable

`PRIMARY KEY ... DEFERRABLE` used to keep the key while dropping the
attribute: `condeferrable` read false, and the key was checked on every row
written, so `update t set a = a + 1` over keys 1 and 2 failed 23505 where
PostgreSQL 15.19 succeeds. A primary key was always the row's storage `_id`,
whose index is unique per write. A deferrable one now leaves the rows a
generated `_id` and is judged as a deferrable UNIQUE is: at the end of the
statement, or at COMMIT when it is `INITIALLY DEFERRED` or set deferred. The
catalogs report it as the primary key. Fixes RP-281
(`tests/backlog/test_rp_top10_found.py::test_a_deferrable_primary_key_is_deferrable`,
`::test_a_deferrable_primary_key_is_checked_as_postgres_checks_it`,
`::test_an_initially_deferred_primary_key_is_judged_at_commit`).

#### Fixed

- `CREATE TABLE` with a column-level or table-level `PRIMARY KEY ...
  DEFERRABLE [INITIALLY DEFERRED]`, and `ALTER TABLE ... ADD PRIMARY KEY ...
  DEFERRABLE`, record and enforce the deferral. The key's columns are NOT
  NULL, duplicates in existing rows are refused as PostgreSQL refuses them,
  and a second primary key is 42P16.
- `pg_constraint` (`contype` `p`, `condeferrable`, `condeferred`),
  `pg_get_constraintdef` (`PRIMARY KEY (a) DEFERRABLE`), `pg_index`
  (`indisprimary`, and `indimmediate` false for this key and for every
  deferrable UNIQUE, which used to read true), psql's `\d` and
  `information_schema.table_constraints` match 15.19.
- A foreign key that names no columns and references a table whose primary
  key is deferrable is refused with 55000 `cannot use a deferrable primary
  key for referenced table`, as on PostgreSQL.

#### Changed

- A table with a deferrable primary key keeps a generated `_id`, so read
  through `secantusd-rs` as a collection its `_id` is that generated value,
  not the key (Joe's call, 2026-10-27). A table whose primary key is not
  deferrable keeps the key as `_id`, as before. The key is stored as a
  unique constraint carrying `primary: true` in `__sql_catalog__`, which
  only this server writes; the Python servers do not read the flag.
