### A RETURN with a value is refused where the routine cannot return one

PostgreSQL's PL/pgSQL grammar checks a RETURN as it reads it: in a routine
returning void (a DO block included) an expression after RETURN is 42804
`RETURN cannot have a parameter in function returning void`, and a
procedure, a set-returning function and a function with OUT parameters each
have their own refusal. The check comes before the expression is parsed, so
it is reported ahead of a later syntax error. The Rust PostgreSQL server ran
such a RETURN, or reported the later syntax error instead; it now refuses it
at CREATE FUNCTION and before a DO block runs, at the token after the RETURN.

#### Fixed

- `RETURN <expr>` in a void function, DO block, procedure, set-returning
  function or function with OUT parameters is refused as PostgreSQL 15
  refuses it, with its SQLSTATE, message, hint and position.

Fixes RP-960 (tests/backlog/test_rp_found_rp619.py::test_return_with_a_value_in_a_void_routine,
tests/backlog/test_rp_found_rp619.py::test_return_expression_by_routine_shape)
