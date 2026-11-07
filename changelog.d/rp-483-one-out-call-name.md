### Rust PG server: a select-list set-returning call is named by its function, and column notation over a base type is 42809

A set-returning call in a select list took its FROM form's column name:
`select gtt()` over `gtt() returns table (t int)` was the column `t`, and
`select json_array_elements('[1,2]')` was `value`. PostgreSQL's
`FigureColname` names the call after its function. A function with one OUT
parameter returns that parameter's type, a scalar, but its set-returning
call was handled as a one-field record, so `gtt() + 1` was refused,
`gtt()::text` was `(5)` and `(gtt()).t` answered 5. Column notation
(`(expr).field`, `(expr).*`) over any base type is now refused when the
statement is analysed, as `ParseFuncOrColumn` refuses it.

#### Fixed

- `select gtt()` is the column `gtt`, `select json_array_elements(...)`,
  `json_array_elements_text(...)` and `jsonb_array_elements(...)` are named
  after their functions, as on PostgreSQL 15.19.
- A set-returning function with one OUT column is a scalar in an
  expression: `gtt() + 1` is 6, `gtt()::text` is `5`, and `select gtt from
  (select gtt()) s` reads it.
- `(expr).field` over a value whose type is a built-in base type or an
  array is 42809 `column notation .x applied to type integer, which is not
  a composite type` at the operand, wherever it is written and whatever
  the rows (a NULL, a column, `(gto()).t`, `(gtt()).t`, `(unnest(...)).x`,
  `(generate_series(...)).x`). Each was 0A000 `field selection on a
  non-record value`, a NULL, or the value. `(expr).*` over one is 42809
  `type integer is not composite`. A composite's or a record's field
  still reads.
  Fixes RP-483 (tests/backlog/test_rp_function_bodies.py::test_one_out_parameter_call_in_the_select_list,
  tests/backlog/test_rp_function_bodies.py::test_column_notation_on_a_base_type,
  tests/backlog/test_rp_function_bodies.py::test_set_returning_target_is_named_by_its_function)
- Filed RP-485, found while working it: a call qualified by a schema that
  does not exist (`nosuch_schema.f()`, or `pg_temp.f()` before the session
  has a temp schema) is 42883 here, not PostgreSQL's 3F000 `schema ... does
  not exist`.
