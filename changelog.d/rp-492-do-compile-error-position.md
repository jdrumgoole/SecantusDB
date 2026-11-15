### A DO block's syntax error points at its token

A DO block with a DECLARE section or a label is compiled by the PL/pgSQL
function interpreter, and a syntax error in it reached the client with the
right message but no position. It now carries the statement position
PostgreSQL gives, at the token the grammar stopped on: `do $$ declare x
int; begin raise notice 'z' end $$` points at the `end`, and a mismatched
end label at the label.

#### Fixed

- Rust PG server: a grammar error in a DO block compiled by the function
  interpreter carries its statement position. libpg_query drops
  PL/pgSQL's internal error position, so the failing token is found again
  as the end of the shortest token prefix of the body that fails with the
  same message.
  Fixes RP-492 (tests/backlog/test_rp_do_compile_errors.py::test_compile_error_has_a_position)
