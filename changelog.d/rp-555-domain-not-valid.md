### Rust PG server: `ALTER DOMAIN ... ADD CONSTRAINT ... NOT VALID` skips the stored rows, and `VALIDATE CONSTRAINT` checks them

A domain CHECK added `NOT VALID` is now added without checking the values
already stored in the domain's columns, as PostgreSQL 15.19's
`AlterDomainAddConstraint` does. Its `pg_constraint.convalidated` is false
and `pg_get_constraintdef` ends in `NOT VALID`. Every new value is still
checked against it. `ALTER DOMAIN ... VALIDATE CONSTRAINT`, which was
refused before, checks the stored values against that one constraint
(`validateDomainConstraint`) and then marks it validated.

#### Fixed

- `alter domain dnv add constraint big check (value > 5) not valid`, with a
  `dnv` column holding 1, now succeeds with `convalidated` false. It used to
  fail with 23514 `column "a" of table "fnv" contains values that violate
  the new constraint`.
- `alter domain dnv validate constraint big` now answers that same 23514
  while a stored value fails the check, and `ALTER DOMAIN` once none does.
  An unknown name is 42704 `constraint "x" of domain "dnv" does not exist`.

Fixes RP-555 (tests/backlog/test_rp_found_rp529.py::test_domain_constraint_not_valid, tests/backlog/test_rp_found_rp529.py::test_domain_constraint_not_valid_then_validate)
