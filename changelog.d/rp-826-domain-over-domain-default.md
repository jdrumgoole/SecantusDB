### Rust PG server: a domain over a domain takes its base domain's DEFAULT

A column of a domain declared over another domain now gets the inner
domain's DEFAULT when an INSERT or ADD COLUMN leaves it out, as on
PostgreSQL 15.19. It was NULL.

#### Fixed

- `create domain dx as int default 42; create domain dx2 as dx` then an
  INSERT omitting a `dx2` column stored NULL; PostgreSQL stores `42`. Like
  `DefineDomain`, `CREATE DOMAIN` now copies the base domain's default into
  the new domain when it has no DEFAULT clause of its own. The default is
  copied once, at CREATE DOMAIN: a later `ALTER DOMAIN dx SET DEFAULT` does
  not move `dx2`'s default, and an explicit DEFAULT (`DEFAULT NULL`
  included) replaces the inherited one. `information_schema.domains`
  lists the copy as `domain_default`, as PostgreSQL does.
- Fixes RP-826 (tests/backlog/test_rp_found_rp799.py::test_a_domain_over_a_domain_takes_its_default);
  regression test
  tests/backlog/test_rp_found_rp799.py::test_a_domain_over_a_domain_copies_its_default_at_create.
