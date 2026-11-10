### Rust PG server: a column retyped into a domain is one of the domain

`ALTER TABLE ... ALTER COLUMN ... TYPE <domain> USING ...` on `secantusd-pg`
now makes the column a column of the domain, as `CREATE TABLE` and `ADD
COLUMN` declare one. PostgreSQL's `ATExecAlterColumnType` sets `atttypid` to
the domain. Before, the column was stored under the domain's name with no
domain tag.

#### Fixed

- An `INSERT` that gives no value for the retyped column fills it with the
  domain's `DEFAULT`. Before, it was NULL. A `DEFAULT` the column declared
  itself stays.
- `information_schema.columns` gives the domain's base type as `data_type`
  and names the domain in `domain_name`. Before, it read `USER-DEFINED` with
  no domain. A column retyped from a domain to the same domain now reads the
  same.

The cast to the domain still checks every stored value against the domain's
`CHECK`, and `DROP DOMAIN` still refuses with 2BP01, as before.

Fixes RP-834 (tests/backlog/test_rp_found_rp824.py::test_a_column_retyped_into_a_domain_is_one_of_it,
crates/secantus-pgplan/src/tests.rs::a_column_retyped_into_a_domain_is_one_of_it)
