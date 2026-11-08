### Rust PG server: CREATE FUNCTION and CREATE PROCEDURE look their types up

`create function f(x nosuch) returns int language sql as $$ select 1 $$`
and the same CREATE PROCEDURE used to be created over a type nothing could
name. On PostgreSQL 15.19, `CreateFunction` looks up every type the
signature names: each parameter's type in
`interpret_function_parameter_list`, then the result's type in
`compute_return_type`. A type that is not there is 42704. A SQL or
PL/pgSQL routine now gets the same answer here.

#### Fixed

- A parameter whose type does not exist is 42704 `type nosuch does not
  exist`, with no position. That covers IN, OUT, INOUT, VARIADIC and
  `RETURNS TABLE` columns, in a function or a procedure. The name is shown
  as `TypeNameToString` writes it: unquoted, qualified as written, and with
  `[]` for an array (`type nosuch[] does not exist`, `type s.nosuch does
  not exist`).
- A result type that does not exist, plain or `SETOF`, is 42704 `type
  "nosuch" does not exist` (quoted, and also with no position). PostgreSQL
  creates a shell type for an unknown result only in a C or internal
  function, which this server already did for `LANGUAGE internal`.
- A type qualified by a schema that does not exist is 3F000 `schema "s"
  does not exist`.
- Parameter types are checked first, then the result type, and both before
  the result is reconciled with the OUT parameters and before the body is
  checked, in PostgreSQL's order.
- A routine may still name a relation's row type: a table's, a view's, a
  temporary table's (bare or as `pg_temp.t`) or a system catalog's
  (`pg_class`, `information_schema.tables`). It may also name a domain, an
  enum, a composite type, a pseudo-type, a `%TYPE` reference, or an array
  of any of these.

Fixes RP-566 (tests/backlog/test_rp_found_rp546.py::test_routine_over_an_undefined_type_is_refused,
tests/backlog/test_rp_found_rp546.py::test_routine_signature_types_are_looked_up).
Files RP-573: a PL/pgSQL `DECLARE` of a type that does not exist is still
accepted at CREATE FUNCTION.
