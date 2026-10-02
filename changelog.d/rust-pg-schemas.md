### Rust PostgreSQL server: schema-qualified tables

The Rust PG server dropped the schema from every relation name, so `CREATE TABLE test_schema.users` was silently created in `public` and collided with `public.users`. Relations are now schema-qualified as in PostgreSQL 15, using the same catalog keys as the Python server (a non-public relation is `schema.name`), so each server reads the other's tables.

#### Fixed

- `schema.table` works in CREATE / DROP / ALTER / RENAME / TRUNCATE, DML with RETURNING and ON CONFLICT, joins and subqueries, serial and identity sequences, UNIQUE / CHECK / FOREIGN KEY, indexes and views.
- Unqualified names resolve through `search_path`, and an unqualified CREATE lands in the path's first existing schema. A missing schema answers `3F000`.
- `DROP SCHEMA` RESTRICT refuses a non-empty schema with `2BP01`, and CASCADE drops its contents.
- The catalogs report the real schema: `pg_class` / `pg_namespace`, `pg_tables`, `pg_indexes`, `pg_views`, `pg_constraint`, `information_schema`, regclass and `to_regclass`.
- `nextval('s.seq')` used the public sequence of the same name. It now resolves the schema.
- `information_schema.sequences` no longer lists identity-column sequences, matching PostgreSQL 15.
- Corpus `schemas`: 75 lines, 0 divergences. Two cross-server tests prove each server reads the other's schema tables. The SQLAlchemy dialect suite against the Rust server went from 421 passed / 782 errors to 811 passed / 0 errors.
