### Rust PG server: a schema's domain names its CHECK after the domain alone

An unnamed CHECK on a domain in a schema took the qualified name. After
`create domain s.sd as int check (value < 100)`, `select 200::s.sd`
reported `violates check constraint "s.sd_check"`. PostgreSQL 15.19 names
the constraint `sd_check`, because `domainAddConstraint` picks the name from
the domain's own name, and the schema is not part of that name.

#### Fixed

- An unnamed domain CHECK is now named `<domain>_check`, then
  `<domain>_check1` and so on, after the domain's unqualified name. This
  covers `CREATE DOMAIN` and `ALTER DOMAIN ... ADD CHECK` for a domain in
  any schema. So `pg_constraint.conname`, the 23514 message and `ALTER
  DOMAIN ... DROP CONSTRAINT sd_check` all use the name PostgreSQL gives
  it. A domain created before this fix keeps the name it was stored with.
- Fixes RP-553 (tests/backlog/test_rp_found_rp529.py::test_schema_domain_check_name),
  with the regression test
  tests/backlog/test_rp_found_rp529.py::test_schema_domain_added_check_name.
