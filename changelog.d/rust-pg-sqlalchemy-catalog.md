### Rust PostgreSQL server: SQLAlchemy reflection works

The SQLAlchemy dialect suite against the Rust PostgreSQL server went from 811 passed / 167 failed to 977 / 1. Every fix was checked against PostgreSQL 15 and pinned in the new `sqlalchemy_catalog` corpus.

#### Fixed

- `pg_table_is_visible` always returned true, so table and view listings included `information_schema` relations. It now follows `search_path`.
- Index names are per schema, not global. `DROP INDEX s.ix` and `COMMENT ON INDEX s.ix` resolve within that schema.
- `pg_get_constraintdef` quotes identifiers the way PostgreSQL does.
- Constraint comments now appear in `pg_description` / `obj_description`.
- `PRIMARY KEY` column order and `INCLUDE` columns are reported correctly in the catalogs.
- A `DECLARE` cursor now sees rows written earlier in the same transaction.
- Index listing reads inside the open transaction.
- A sequence created in the transaction is visible to `regclass`.
- Casts over joined columns and scalar subqueries in the select list now get PostgreSQL's column names and types.
- `LIMIT $1` inside a scalar subquery no longer fails at Describe.
