### Rust PG server: a domain's DEFAULT is the type's, not its column's

A column of a domain with a DEFAULT has no default of its own on
PostgreSQL: the domain's is the type's (`pg_type.typdefault`), which an
INSERT falls back to when the column has none (`build_column_default`).
The Rust PostgreSQL server copied it into the column and listed it there.

#### Fixed

- `pg_attrdef`, `pg_attribute.atthasdef` and
  `information_schema.columns.column_default` no longer list a domain's
  DEFAULT as its column's: no row, `false` and NULL, as PostgreSQL 15.19
  answers.
- `ALTER TABLE ... ALTER COLUMN ... SET DEFAULT` on a column of a domain
  makes the default the column's own, which a later `ALTER DOMAIN ... SET
  DEFAULT` no longer replaces (it did).
- `ALTER COLUMN ... DROP DEFAULT` on a column of a domain removes only the
  column's own default, so an INSERT takes the domain's again, as it
  stands then and as a later `ALTER DOMAIN` moves it (it gave NULL).
- Fixes RP-825 (tests/backlog/test_rp_found_rp799.py::test_a_domains_default_is_not_its_columns,
  tests/backlog/test_rp_found_rp799.py::test_a_columns_drop_default_leaves_its_domains).
  Crate test: `secantus-pgplan` `a_columns_set_and_drop_default_leave_its_domains`.

#### Filed

- RP-839: a column `CREATE TABLE ... (LIKE t)` copies from a column of a
  domain is typed by the domain's base, not the domain.
