### Rust PG server: an operator beside `current_user` or `current_schema` judges it as `name`

The role, catalog and schema keyword functions (`current_user`,
`current_role`, `user`, `session_user`, `current_catalog`,
`current_schema`) are now typed `name` when operators are resolved, as
`transformSQLValueFunction` types them in PostgreSQL. Before this, the
operator check did not type them at all, so an operator over one was
judged on its value's text.

#### Fixed

- `select 1 + current_user` is 42883 `operator does not exist: integer +
  name`, and `select current_schema + 1` is `name + integer`, as on
  PostgreSQL 15.19, where both said `text`.
- `current_user = 1`, `= true`, `= 1.5` and `in (1, 2)` are 42883 `name =
  ...` where they were 0A000 `comparing string with ... is not supported
  yet`. `current_user = '{}'::jsonb`, `= gen_random_uuid()` and `= date
  '2020-01-01'` are 42883 where they returned a row. `-current_user` is `-
  name` where it was `integer - text`.

Fixes RP-448 (tests/backlog/test_rp_fromless_analysis.py::test_keyword_function_operand_is_name)
