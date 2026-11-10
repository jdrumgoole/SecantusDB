### Rust PG server: a column retyped off its domain is no longer one of the domain

`ALTER TABLE ... ALTER COLUMN ... TYPE` on `secantusd-pg` now gives the
column its new type outright, as PostgreSQL's `ATExecAlterColumnType` does.
Before, a column retyped off a domain stayed tagged with that domain, so the
domain still applied to every write.

#### Fixed

- A column retyped from a domain to a plain type no longer enforces the
  domain's `CHECK` or `NOT NULL`. `information_schema.columns` no longer
  names the domain, and `DROP DOMAIN` no longer reports the column as
  depending on it.
- The domain's `DEFAULT` no longer fills the retyped column. A `DEFAULT` the
  column declared itself stays.
- The retyped column takes the collation of the statement's `COLLATE`
  clause, or else the new type's collation, as `GetColumnDefCollation` gives
  it. Before, a column kept the collation it had, whether from its own
  `COLLATE` or from its old domain, and the `COLLATE` clause of `ALTER
  COLUMN TYPE` was ignored. A collation that does not exist is 42704, and a
  `COLLATE` on a type with no collation is 42804, reported before the cast
  check.

Fixes RP-824 (tests/backlog/test_rp_found_rp799.py::test_a_column_retyped_off_its_domain_drops_it,
tests/backlog/test_rp_found_rp799.py::test_a_retyped_column_takes_the_new_types_collation_and_default)

Filed while working it: RP-834 (a column retyped into a domain with `USING`
is not one of the domain).
