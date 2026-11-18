### A CREATE FUNCTION compile error carries its compile context

An error PL/pgSQL's grammar raises without a scanner position, such as an
unknown condition name in `RAISE`, now carries PostgreSQL's CONTEXT line
`compilation of PL/pgSQL function "<name>" near line N` on CREATE FUNCTION,
as a DO block already did.

#### Fixed

- `secantusd-pg`: CREATE FUNCTION's position-less PL/pgSQL compile errors
  are framed by `plpgsql_compile_error_callback`'s context line.
  Fixes RP-1040 (tests/backlog/test_rp_found_rp619.py::test_create_function_compile_error_has_context)
