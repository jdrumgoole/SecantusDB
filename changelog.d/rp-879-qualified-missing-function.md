### A missing function is named the way the statement wrote it

When a statement called a function that does not exist and named its schema,
as in `select public.nosuch(1)` or `select postgres.public.nosuch()`, the Rust
PostgreSQL server dropped the schema from the error message and pointed the
error at the bare function name. PostgreSQL names the function exactly as it
was written and points at the first part of that name. The server now does
the same.

#### Fixed

- Rust PG server: a 42883 for a schema-qualified call gives the qualified
  name as written (`function public.nosuch(integer) does not exist`) and
  points at the first part of that name, including the three-part form.
  Fixes RP-879 (tests/backlog/test_rp_qualified_calls.py::test_missing_function_keeps_its_schema)
