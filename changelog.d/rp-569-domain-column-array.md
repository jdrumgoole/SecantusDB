### Rust PG server: an array built from a domain column is the domain's array

PostgreSQL types a column declared with a domain as the domain itself, so
`array[a]`, `array_agg(a)` and `array(select a from t)` over a domain
column are arrays of the domain (`d[]`, its `typarray`). The Rust
PostgreSQL server typed them by the column's base type, and the ARRAY
subquery came back as `text[]` with a `char(n)` domain's blanks lost (`xy`
for `xy `). Each now matches PostgreSQL 15.19: the domain's array type on
the wire, padding included.

#### Fixed

- Over a single-table FROM, a domain column is cast to its domain where an
  array shows its type (an `ARRAY[...]` element, `array_agg`'s argument,
  `pg_typeof`'s, an ARRAY subquery's column), and an ARRAY or scalar
  subquery whose column is a domain, or a domain's array, is cast to that
  type rather than left as an untyped `text` value.
- `pg_typeof(a)` over a domain with a CHECK added `NOT VALID` no longer
  fails with 23514 on a row stored before the CHECK; it now names the base
  type, which is the open RP-655.

Fixes RP-569 (tests/backlog/test_rp_found_rp559.py::test_array_of_domain_column),
also pinned by `secantus-pgplan`'s
`tests::a_domain_column_is_cast_to_its_domain_where_an_array_shows_it`.
Files RP-655 (a cast of a domain column to its own domain checks the
domain), RP-656 (an ARRAY subquery over a `char(n)` column drops its
blanks) and RP-657 (a domain column through a join, a WITH or a set
operation is base-typed in an array).
