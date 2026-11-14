### A PL/pgSQL variable cannot be declared with a pseudo-type

A PL/pgSQL function that declared a variable of a pseudo-type, such as
`anyelement` or `void`, was created as though the type were an ordinary one.
PostgreSQL's compiler refuses such a variable when the function is created,
and the Rust PostgreSQL server now does too: 0A000 `variable "x" has
pseudo-type anyelement`, with the compiler's `near line N` context.
`record` is still a record variable.

#### Fixed

- Rust PG server: `CREATE FUNCTION ... LANGUAGE plpgsql` refuses a DECLARE
  of a pseudo-type (`plpgsql_build_variable`).
  Fixes RP-618 (tests/backlog/test_rp_found_rp573.py::test_declare_of_a_pseudo_type)
