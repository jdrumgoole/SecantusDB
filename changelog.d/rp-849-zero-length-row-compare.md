### Rust PG server: a row comparison is held to its rows' lengths

Two row constructors compared field by field are now checked as
PostgreSQL's `make_row_comparison_op` checks them, before any operator is
looked up: rows of no fields are 0A000 `cannot compare rows of zero
length` at the operator, and rows of different lengths 42601 `unequal
number of entries in row expressions`.

#### Fixed

- `select row() = row()`, `row() <> row()`, `row() <= row()`, `row() in
  (row())`, `row() in (select)` and `row() = any / all (select)` are 0A000
  at the operator, where they answered `t` or `f`. Each item of a row IN
  list is checked in order, after every operand's bare column names
  resolve (`row() = row(nosuch)` stays 42703). `row() is distinct from
  row()` still answers, as `make_row_distinct_op` has no such check.
- `row(1) < row(1, 2)`, `row(1, 2) in (row(1, 2), row(1))` and `row(1)
  is distinct from row(1, 2)` are 42601, where they answered.

Fixes RP-849 (tests/backlog/test_rp_found_rp727.py::test_zero_length_row_comparison_is_0a000)

#### Filed

- RP-875: a row beside an operand that is not a row looks its operator up
  before a missing column is reported (42883 where PostgreSQL is 42703).
