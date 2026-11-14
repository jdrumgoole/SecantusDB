### A routine's parameter and result column names print quoted

`pg_get_function_arguments`, `pg_get_function_identity_arguments` and
`pg_get_function_result` now pass each parameter's and `RETURNS TABLE`
column's name through `quote_identifier`, as PostgreSQL's
`print_function_arguments` and `print_function_rettype` do: a parameter
named `"A b"` or `"select"` prints quoted, so the text reads back as valid SQL.

#### Fixed

- Parameter and TABLE column names in the function-signature helpers are
  quoted when needed.
  Fixes RP-876 (tests/backlog/test_rp_found_rp515.py::test_argument_and_column_names_are_quoted)
