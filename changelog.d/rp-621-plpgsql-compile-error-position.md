### A PL/pgSQL compile error points into the CREATE FUNCTION

When a PL/pgSQL body fails to compile -- an unknown variable, an alias for a
parameter the function does not have, a syntax error in an expression -- the
Rust PostgreSQL server now reports PostgreSQL's own SQLSTATE and places the
error at the offending character of the CREATE FUNCTION statement, the way
`function_parse_error_transpose` does. Before, every such error was 42601
with no position, because libpg_query dropped both. The server now builds
against `secantus-pg-query`, SecantusDB's fork of pg_query 6.2.0, which keeps
the error's `sqlerrcode` and `internalpos`.

#### Added

- `secantus-pg-query`: a fork of pg_query 6.2.0 (published under that name,
  used as `pg_query`) whose `parse_plpgsql_detailed` returns a compile
  error's SQLSTATE and its position in the routine's source.

#### Fixed

- A PL/pgSQL compile error in CREATE FUNCTION carries its real SQLSTATE
  (`variable "$2" does not exist` is 42704) and a position in the statement.
  Fixes RP-621 (tests/backlog/test_rp_found_rp573.py::test_plpgsql_compile_error_in_create_has_a_position)
