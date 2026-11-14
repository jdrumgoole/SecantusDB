### A procedure's definition reads as a procedure's

`pg_get_function_result` of a procedure is now NULL, as PostgreSQL's is,
and `pg_get_functiondef` prints `CREATE OR REPLACE PROCEDURE`, with no
RETURNS line and the body quoted `$procedure$`. Before, a procedure read as
a function returning `record`.

#### Fixed

- Rust PG server: `pg_get_function_result` and `pg_get_functiondef` of a
  user procedure.
  Fixes RP-877 (tests/backlog/test_rp_found_rp515.py::test_procedure_result_and_definition)
