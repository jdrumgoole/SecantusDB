### Rust PG server: a `text` scalar subquery is typed `text`

An uncorrelated scalar subquery is resolved while the statement is
planned, and its value stands in as a parameter declared with the
subquery column's type. Every column type was declared except `text`, so
a `text` subquery reached the expression around it with no type at all.

#### Fixed

- `pg_typeof((select max(v) from t))` over a `text` (or `varchar`, via
  `max(text)`) column is `text`, as on PostgreSQL 15.19, not 42P18 `could
  not determine data type of parameter $1`. The same untyped parameter
  made `concat` and `format` over such a subquery 42P18, made `(select
  max(x) ...) || array['q']` 22P02 `malformed array literal`, and made
  `(select '12') + 1` answer 13 where PostgreSQL has no `text + integer`
  operator (42883).

Fixes RP-707 (tests/backlog/test_rp_found_rp681.py::test_pg_typeof_of_a_text_scalar_subquery,
tests/backlog/test_rp_found_rp681.py::test_a_text_scalar_subquery_is_typed_text)
