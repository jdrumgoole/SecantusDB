### Rust PG server: a scalar subquery can stand under `IS TRUE` or `COLLATE`

`(x = (select 'uu')) is not true` and `x = (select 'uu') collate "C"` are
ordinary expressions to PostgreSQL 15. This server resolved a scalar
subquery only where it could see it, and it did not look inside a boolean
test or a `COLLATE`, so each statement was refused with 0A000 `SubLink is
not supported yet`.

#### Fixed

- A scalar subquery, correlated or not, under `IS [NOT] TRUE`, `IS [NOT]
  FALSE`, `IS [NOT] UNKNOWN` or `COLLATE` is evaluated like any other value,
  in a WHERE or a select list. A `COLLATE` over it changes the comparison
  as it does on PostgreSQL.

Fixes RP-774 (tests/backlog/test_rp_found_rp757.py::test_a_scalar_subquery_under_a_boolean_test_or_collate).
