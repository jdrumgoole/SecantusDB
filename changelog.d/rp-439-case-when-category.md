### Rust PG server: a simple CASE with no `=` for its WHEN is 42883

A simple CASE (`case a when b then ...`) compares its argument to each WHEN
with `=`, and PostgreSQL looks that operator up during parse analysis. When
the WHEN is typed in a different category from the argument, such as an
integer against a boolean or a text against an integer, there is no such
operator. PostgreSQL answers 42883 `operator does not exist` at the WHEN.
Here the comparison was left to the evaluator, which refused it with 0A000
`comparing int32 with boolean using = is not supported yet`. A client that
matches on the error class read that as a missing feature, not a mistake in
its query.

#### Fixed

- `secantusd-pg`: a simple CASE resolves `argument = WHEN` against
  PostgreSQL 15.19's operators when the statement is analysed, whether or
  not a row reaches it. `case 1 when true` is 42883 `operator does not
  exist: integer = boolean`, positioned at the WHEN keyword, and so are
  `case b when 1` over a text `b` and `case a when b` over an integer `a`
  and a text `b`. The first WHEN with no operator is the one named. An
  untyped argument is coerced to text first, so `case 'a' when 1` is `text =
  integer`. Types that `=` joins still compare: `int2` against `int8`,
  `varchar` against `text`, `numeric` against `float8`.

Fixes RP-439 (tests/backlog/test_rp_where_types.py::test_simple_case_when_of_another_category_is_42883).

#### Filed

- RP-441: in a SELECT with no FROM, a missing column inside an expression
  (`select 1 + nosuch`, `coalesce(nosuch, 1)`, any part of a CASE) is 0A000
  `ColumnRef is not supported yet`, and `abs(nosuch)` is 42883 `function
  abs(text)`. PostgreSQL answers 42703 `column "nosuch" does not exist`.
- RP-442: a cast literal's input error is reported after the operator around
  it. `select 'x'::int = true` is the operator's 42883 here. PostgreSQL
  reads the constant first and answers 22P02 `invalid input syntax for type
  integer: "x"`.
