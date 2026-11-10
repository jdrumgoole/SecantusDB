### Rust PG server: a domain over a text type takes its base type's collation

PostgreSQL gives a domain the collation of the type it is defined over, so
a domain over `text` or `varchar` carries `default` and one over `name`
carries `C`. The Rust PostgreSQL server gave every domain no collation at
all. That made `create view v as select a from t` (and `CREATE
MATERIALIZED VIEW`) over a column of such a domain fail with 42P16 `no
collation was derived for column "a" with collatable type text`, which
PostgreSQL accepts. `pg_type.typcollation` of the domain and of its array
type, and `pg_attribute.attcollation` of its columns, read 0 instead of
100 (or 950). `collation for (x)` over a domain-array column was 42804.
A domain now takes its base type's collation, through a domain over a
domain and a domain over an array. Over a domain with no collation,
`collation for` now names the domain in its 42804, as PostgreSQL does.

#### Fixed

- A view or materialized view of a column whose type is a domain over a
  text type is created instead of refused with 42P16.
- `pg_type.typcollation` and `pg_attribute.attcollation` of a domain over
  a collatable type, its array type and its columns are the base type's
  collation.
- `collation for` over a column of a domain, or of a domain's array type,
  answers the base type's collation, and over a domain with none, its
  42804 names the domain.

Fixes RP-769 (tests/backlog/test_rp_found_rp661.py::test_a_view_of_a_text_domain_column, tests/backlog/test_rp_found_rp661.py::test_a_text_domain_has_the_default_collation)
