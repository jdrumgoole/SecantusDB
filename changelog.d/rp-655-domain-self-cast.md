### Rust PG server: a cast of a domain column to its own domain checks nothing

A column declared with a domain is already of that domain, so on
PostgreSQL `a::d` over it is no coercion (`coerce_type` returns the column
as it is) and checks none of the domain's constraints. The Rust PostgreSQL
server checked them, so a row stored before a CHECK added `NOT VALID`
failed with 23514 wherever the cast appeared: in a select list, a WHERE,
an UPDATE's or DELETE's expressions. `pg_typeof(a)` and `array[a]` over
such a domain fell back to the base type. Each now matches PostgreSQL
15.19.

#### Fixed

- A cast of a domain column to its own domain, over a single-table FROM or
  an UPDATE's or DELETE's target, is a relabel: it is typed as the domain
  and reads the value as before (a `char(n)` domain's blanks included), but
  checks no constraint. So is the cast a scalar or ARRAY subquery's value
  gets to its column's domain.
- `pg_typeof(a)`, `array[a]` and `array_agg(a)` over a domain with a
  `NOT VALID` CHECK name the domain and its array, as they already did for
  every other domain.

Fixes RP-655 (tests/backlog/test_rp_found_rp569.py::test_cast_of_a_domain_column_to_its_domain_checks_nothing),
also pinned by `secantus-pgplan`'s
`tests::a_cast_of_a_domain_column_to_its_own_domain_is_a_relabel`.
Files RP-664 (a write re-checks a domain value it does not coerce into the
domain, so an UPDATE of another column fails) and RP-665 (a scalar
subquery over a domain column is typed by the domain's base).
