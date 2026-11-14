### A PL/pgSQL declaration's missing type is reported before a later syntax error

PostgreSQL compiles a PL/pgSQL body in one pass, looking each declaration's
type up as it reaches it, so a function that declares a type that does not
exist and also has a syntax error further down is refused for the missing
type. The Rust PostgreSQL server parsed the whole body first and reported the
syntax error instead. It now finds the declarations the compiler reached
before the error -- in the DECLARE section or a nested block's -- and checks
their types first, for CREATE FUNCTION and DO alike.

#### Fixed

- `secantus-pg`: a PL/pgSQL body that does not parse has the declarations
  before its error type-checked first (`plpgsql_fn::compiled_declarations`
  cuts the body after the last statement or declaration that still compiles
  and closes what is open there), so `declare x nosuch; begin return 1 +;
  end` is 42704 `type "nosuch" does not exist` at the type, as on
  PostgreSQL 15.19.
  Fixes RP-619 (tests/backlog/test_rp_found_rp573.py::test_declare_type_found_before_a_later_syntax_error)
- Filed RP-958 (a PL/pgSQL expression's syntax error is placed at the end of
  the statement), RP-959 (a DO block refuses `#` compile options) and RP-960
  (a RETURN with a value in a void routine is accepted).
