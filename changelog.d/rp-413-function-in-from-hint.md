### A missing column's hint searches a function in FROM

A misspelt column in a query with a function in FROM now gets PostgreSQL's
`Perhaps you meant to reference the column ...` hint: the search for the
closest column reads the function's columns from its result type, as
PostgreSQL's range table holds them, instead of giving up.

#### Fixed

- Rust PG server: `select * from generate_series(1,2) g where gg = 1` hints
  `"g.g"`, and a function beside a table no longer suppresses the table's
  hint. Fixes RP-413 (tests/backlog/test_rp_column_errors.py::test_function_in_from_hint)
