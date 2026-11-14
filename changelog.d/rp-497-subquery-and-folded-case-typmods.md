### A scalar subquery and a constant-folded CASE keep their type modifier

A scalar subquery's column now carries its select list's type modifier in every reader, as PostgreSQL's `exprTypmod` of a SubLink does: `select (select 2.5::numeric(5,2))` describes `numeric(5,2)`, and so does a `timestamp(3)` or `varchar(n)` one, and one that returns no row. A CASE whose WHEN folds to a constant -- `case 1 when 1 ...`, `case when 1 = 1 ...` -- is simplified before a portal's Describe, a populating CREATE TABLE AS and a SELECT from a view read its modifier, as `eval_const_expressions` simplifies it; a prepared statement's Describe and a view's columns still read the unfolded CASE.

#### Fixed

- Rust PG server: a scalar subquery's numeric, time and timestamp modifier, and a CASE folded by a computed constant condition, are described as PostgreSQL describes them.
  Fixes RP-497 (tests/backlog/test_rp_ctas_subquery.py::test_typmod_from_scalar_subquery_and_computed_constants)
