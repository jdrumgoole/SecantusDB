### Rust PG server: a `char(n)` value evaluated per row compares with `text` as PostgreSQL resolves it

A comparison evaluated row by row gave two wrong answers when it compared a
`char(n)` value with `text`. This covers a select list, and a WHERE over a
VALUES list or a set operation. The cause in both cases was that the
per-row evaluator lost a type that PostgreSQL settles while it analyses the
statement.

#### Fixed

- An IN list with more than one member that reads no column is one array
  comparison over the list's common type, as in PostgreSQL's
  `transformAExprIn`. Those members are coerced to that type first. Against
  a `char(n)` operand the common type is `bpchar`, so
  `f in ('uu  '::text, 'tt')` is true for `uu`, as on PostgreSQL 15.19.
  Before this fix the evaluator compared each member with its own `=`,
  which kept the `text` member's blanks, so the row did not match. A
  member that reads a column is still a plain `=`, and so is a list with
  only one constant member.
- A scalar subquery's value keeps its column's type when a row is
  evaluated. `f = (select 'uu  '::text)` is `texteq` over the `rtrim`med
  column, false for every row, and `pg_typeof((select 'uu  '))` is `text`.
  Before this fix the type that planning declared for the subquery's
  parameter was gone by the time a row was evaluated. The value was then
  read as untyped and compared as `bpchar`, and `pg_typeof` was 42P18.

#### Found

- RP-806: an IN list member that the list's common type cannot read (the
  `'x'` in `1 in (1, 'x')`) is 22P02 on PostgreSQL even when an earlier
  member matches or there is no row. This server compares the members one
  at a time and stops at the first match, so it raises no error.

Fixes RP-792 (tests/backlog/test_rp_found_rp775.py::test_a_char_operand_against_text_per_row)
