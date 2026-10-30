### Rust PostgreSQL server: an aggregate over a joined subquery keeps the subquery's LIMIT and OFFSET

`select count(*) from (select ... from m0 join m1 on ... limit 3) s` used
to count every joined row (18) instead of the three the LIMIT keeps, and
OFFSET was dropped in the same way. A joined FROM subquery with a LIMIT or
OFFSET is now planned as a materialised source, so the clause applies
before the aggregate does, as in PostgreSQL 15.19.

#### Fixed

- Fixes RP-307 (tests/backlog/test_rust_postgres_gaps.py::test_an_aggregate_over_a_joined_subquery_keeps_its_limit)
