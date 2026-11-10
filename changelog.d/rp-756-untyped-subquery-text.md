### Rust PG server: a subquery over an untyped literal is `text` outside it

A scalar subquery whose column is an untyped string literal or a NULL --
`(select 'a')`, `(select null)` -- is now `text` to the expression around
it, as PostgreSQL resolves a subquery's untyped targets
(`resolveTargetListUnknowns`). It was read as `unknown` and coerced to the
other side's type, so `(select '2020-01-01') < now()` answered `true`,
`x = (select '1')` over an integer column matched, `coalesce(1, (select
'a'))` answered `1` and `(select null) = 1` answered NULL, where
PostgreSQL 15.19 raises 42883 or 42804 for each. A FROM subquery's or a
CTE's untyped column is `text` to the operator check the same way.

#### Fixed

- A comparison, `IN`, `BETWEEN`, `NULLIF`, `CASE`, `COALESCE` or
  `GREATEST` / `LEAST` over a scalar subquery of an untyped literal or
  NULL is 42883 / 42804 as on PostgreSQL, through a FROM subquery or a
  set operation of untyped arms inside it as well.

Fixes RP-756 (tests/backlog/test_rp_found_rp707.py::test_an_untyped_literal_subquery_is_text_outside_it).
Files RP-765 (a FROM subquery that is a set operation is not typed by the
operator check; tests/backlog/test_rp_found_rp756.py).
