### Tables get real schemas — and NOT LIKE stops lying

Relations in a user schema are now first-class: `CREATE TABLE
test_schema.users` coexists with `public.users`, resolves qualified from any
statement (DML, views, sequences, indexes, comments, foreign keys), reflects
under its own `pg_namespace` row, and is invisible to unqualified lookups —
`pg_table_is_visible` now enforces the default search path, exactly like real
Postgres. `DROP SCHEMA … CASCADE` takes the schema's tables with it, creating
into a nonexistent schema raises `3F000`, and a cross-schema foreign key
renders its target as separately-quoted identifiers in
`pg_get_constraintdef`. Internally a schema-qualified relation stores under a
dotted catalog key (`test_schema.users`) — the same mapping user-defined
types adopted — so the dual-protocol Mongo view addresses the backing
collection as `db["test_schema.users"]`.

With that in place the SQLAlchemy compliance gauge's `schemas` capability
opens, unlocking the suite's entire schema-qualified surface: **978 of 978
executed tests pass (100%)**, up from 731 executed before, still with zero
failures and zero errors.

Standing the schema surface up flushed out a genuine wrong-answer bug:
sqlglot parses `NOT LIKE` as `Like(negate=True)` rather than wrapping it in
`NOT`, and both the pushdown translator and the per-row evaluator ignored the
flag — so `WHERE n NOT LIKE 'pg_%'` silently behaved as `LIKE`. Both engines
now honor the negation.

#### Added

- `sql/planner.py` (`qualified_table_name`) + resolution sites across
  `engine`/`executor`: schema-qualified tables, views, and sequences stored
  under dotted catalog keys; `3F000` on unknown target schemas; DROP SCHEMA
  CASCADE drops contained tables.
- `sql/virtual.py`: relations split into (schema, relname) for `pg_class` /
  `information_schema` reflection; `pg_temp_1` namespace row; cross-schema
  FK targets quoted per part in `pg_get_constraintdef`.
- `sql/planner.py`: `pg_table_is_visible` lowers to a search-path check
  (default namespaces + the session's own temp tables).
- `sqlalchemy_validation/`: the `schemas` capability opens; the runner
  pre-provisions `test_schema` / `test_schema_2` (SQLAlchemy's documented
  DBA setup step).

#### Fixed

- `sql/planner.py` + `sql/scalar.py`: `NOT LIKE` behaved as `LIKE` — sqlglot
  encodes the negation as `Like(negate=True)`, which both engines ignored.
- `sql/executor.py`: `COMMENT ON` a schema-qualified table or column landed
  on the same-named public relation.
- `sql/planner.py`: an auto-named foreign key on a schema-qualified table
  minted `schema.table_col_fkey` instead of PG's `table_col_fkey`.
