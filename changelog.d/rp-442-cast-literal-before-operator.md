### A cast literal's bad input is reported before the operator around it

PostgreSQL reads a constant cast such as `'x'::int` as the cast is analysed, before it looks up the operator the cast sits under. So `select 'x'::int = true` is the integer input error, not "operator does not exist: integer = boolean". The Rust PostgreSQL server now reports the same error at the same position, in an operator and in a simple CASE's argument.

#### Fixed

- Rust PG server: `select 'x'::int = true`, `select 1 = 'x'::uuid`, `select '2020-13-01'::date = 1` and `case 'x'::uuid when 1` are 22P02 / 22008 at the literal, not 42883.
  Fixes RP-442 (tests/backlog/test_rp_fromless_analysis.py::test_cast_literal_input_error_before_operator)
