### Rust PG server: a routine's argument list prints each parameter's mode

`pg_get_function_arguments`, `pg_get_function_identity_arguments` and the
argument list in `pg_get_functiondef` now print a routine's parameters as
PostgreSQL 15.19's `print_function_arguments` does: every declared
parameter in order, with its mode.

#### Fixed

- A procedure's IN parameters print as `IN a integer`, as PostgreSQL marks
  them so the text cannot be read as DROP PROCEDURE's argument list:
  `create procedure gk(a int, inout b text)` prints `IN a integer, INOUT b
  text` (it printed `a integer, b text`).
- OUT and INOUT parameters, of a function or a procedure, print with their
  mode and in their declared place. An OUT parameter was left out
  altogether: `f(a int, out b int, inout c text default 'q')` printed `a
  integer, c text DEFAULT 'q'::text`, now `a integer, OUT b integer, INOUT
  c text DEFAULT 'q'::text`. A default stays on its input parameter, and
  TABLE columns stay in the result.
- Fixes RP-515 (tests/backlog/test_rp_found_rp508.py::test_procedure_arguments_print_their_modes),
  with the regression test
  `tests/backlog/test_rp_found_rp508.py::test_out_and_inout_arguments_print_their_modes`.
- Filed while working it: RP-876 (argument and TABLE column names are not
  quoted) and RP-877 (a procedure's `pg_get_function_result` is `record`,
  not NULL, and its `pg_get_functiondef` prints a FUNCTION).
