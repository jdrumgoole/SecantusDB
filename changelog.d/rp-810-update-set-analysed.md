### Rust PG server: an UPDATE's SET expressions are checked whatever the rows

`update t set b = (i is true) where false` over an `int` column answered
`UPDATE 0`, as did `(i and true)` and `(not i)`. PostgreSQL 15.19 refuses
each one with 42804 `argument of IS TRUE must be type boolean, not type
integer`, because `transformUpdateStmt` transforms every SET expression
when the statement is analysed. Here a SET expression was only judged when
a row's new value was computed, so the same statement failed or succeeded
depending on the data.

#### Fixed

- The analysis pass now walks an UPDATE's SET expressions after its WHERE,
  as it already walked a SELECT's targets, a DELETE's WHERE and an
  INSERT's VALUES. A type error in a SET expression is refused whether or
  not a row reaches it.

Fixes RP-810 (tests/backlog/test_rp_found_rp808.py::test_an_update_set_expression_is_coerced_to_boolean)
