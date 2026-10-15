### Rust PostgreSQL server: backlog batch 3

More of the Rust PostgreSQL server's backlog, each fix measured against
PostgreSQL 15.19 and pinned by the backlog test that found it.

#### Fixed

- `pg_typeof` of a column of a domain type names the domain (`rp38_d`),
  where it printed the base type (`integer`); qualified or not, in a select
  list or a WHERE. Part of RP-038
  (`tests/backlog/test_rust_postgres_gaps.py::test_pg_typeof_names_the_domain`).

#### Added

- Corpus `domain_typeof`.
