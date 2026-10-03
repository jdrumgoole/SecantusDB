### Rust PostgreSQL server: psycopg back to zero failures, COMMIT no longer rebuilds the catalog

The psycopg suite against the Rust PostgreSQL server had regressed from 1 failure (2026-09-18) to 61. It is now 5,544 passed / 0 failed. Every fix is checked against PostgreSQL 15 and pinned in the `b49_psycopg` corpus.

#### Fixed

- An untyped literal or parameter compared with a range or multirange takes the range's type (`'empty' = $1::int4range`).
- An explicit cast to `bpchar` with no length keeps trailing blanks (`chr(32)::bpchar` is `' '`).
- An untyped argument no longer forces a text overload (`array[set_byte('x', 0, $2)]`).
- Temporary tables:
  - a temp table can `REFERENCES` itself;
  - constraint errors on a temp table report its bare name, with `pg_temp_N` as the schema.
- `SET client_encoding` inside a block keeps working after COMMIT.

#### Performance

- COMMIT and ROLLBACK rebuilt every connection's catalog view, even when the transaction changed no catalog. 60 `select; commit` over 100 tables took 31.9 s; they now take 0.4 s. The full psycopg run dropped from 1,539 s to 1,010 s.
