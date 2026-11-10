### Rust PG server: a scalar subquery over a domain column is of the domain

`(select a from t)` over a column declared with a domain is now of that
domain, as an EXPR SubLink takes its one column's type in PostgreSQL. It
was typed by the domain's base. The value still describes as the base
type, as `SendRowDescriptionMessage` sends it, and checks none of the
domain's constraints: no row is a NULL of a NOT NULL domain, and a cast of
the subquery to its own domain is no coercion.

The type a CASE, COALESCE, GREATEST, LEAST or VALUES column resolves to
now follows `select_common_type`'s rule for domains: inputs all of exactly
one type keep it, and otherwise each is resolved by its domain's base. A
CASE with no ELSE has an implicit untyped `ELSE NULL`, and NULLIF is typed
by its `=`'s left input, the base type's.

#### Fixed

- `pg_typeof((select a from t655 where b < 0))` is `d655`, not `integer`,
  also with no row, through a set operation, a FROM subquery, a WITH item
  or a join, and per row when the subquery is correlated.
  `ARRAY[(select a ...)]` is `d655[]`.
- `(select a from t655 where b < 0)::d655` returns the stored value, where
  it raised the domain's `NOT VALID` CHECK (23514).
- `pg_typeof(coalesce('5'::d655, 0))` and `pg_typeof(case when true then
  '5'::d655 end)` are `integer`, not `d655`.

Fixes RP-665 (tests/backlog/test_rp_found_rp655.py::test_scalar_subquery_over_a_domain_column_is_of_the_domain)

#### Filed

- RP-764: a CASE, COALESCE, GREATEST or LEAST over domain columns alone
  (`coalesce(a, a)`) is still typed by the domain's base
  (`tests/backlog/test_rp_found_rp665.py`).
