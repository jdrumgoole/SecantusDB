### A DROP or ALTER FUNCTION signature resolves `%TYPE`

`drop function f(t.v%type)` and `alter function f(t.v%type) rename to g`
now find the routine whose argument has the column's type, with the same
NOTICE CREATE FUNCTION gives, as PostgreSQL's `LookupFuncWithArgs` does.

#### Fixed

- A `%TYPE` argument in a DROP / ALTER FUNCTION, PROCEDURE or ROUTINE
  signature is the column's type, not a type named `t.v`.
  Fixes RP-830 (tests/backlog/test_rp_found_rp822.py::test_a_signatures_percent_type_is_resolved)
