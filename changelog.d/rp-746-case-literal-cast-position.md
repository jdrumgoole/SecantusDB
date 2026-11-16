### A cast over a CASE of string literals reports its input error without a position

When every arm of a CASE, COALESCE, GREATEST or LEAST is an untyped string
literal, PostgreSQL resolves the expression to `text` and the cast over it
reads that text as the row runs, so an input error such as
`(case when k = 1 then 'x' else '5' end)::int` carries no position. The Rust
PostgreSQL server pointed the error at the literal; it now reports it
unpositioned, as PostgreSQL 15.19 does, while a CASE with a typed arm still
places the error at the literal it coerced.

#### Fixed

- Rust PG server: a 22P02 / 22003 input error from a literal arm of an
  all-literal CASE, COALESCE, GREATEST or LEAST has no position.
  Fixes RP-746 (tests/backlog/test_rp_found_rp732.py::test_a_cast_over_a_case_of_literals_reports_no_position)
