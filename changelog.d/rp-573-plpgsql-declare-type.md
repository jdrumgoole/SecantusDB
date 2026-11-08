### Rust PG server: a PL/pgSQL DECLARE looks its type up at CREATE FUNCTION

`create function f() returns int language plpgsql as $$ declare x nosuch;
begin return 1; end $$` used to be created, and the missing type was only
reported (if at all) when the function was called. On PostgreSQL 15.19
`plpgsql_validator` compiles the body at CREATE FUNCTION, and compiling a
declaration looks its type up (`read_datatype`, `parse_datatype`). A DO
block is compiled the same way before any of it runs. A PL/pgSQL routine
or DO block now gets the same answer here.

#### Fixed

- A declared type that does not exist is 42704 `type "nosuch" does not
  exist`, at the type's text in the CREATE or DO statement (placed as
  `function_parse_error_transpose` places it, through a `'...'` body with
  `''` escapes too). The name is shown as `TypeNameToString` writes it,
  with `[]` for an array, and a qualifier naming no schema is 3F000. This
  covers nested blocks, `CONSTANT` and `NOT NULL` declarations, a cursor's
  argument types, trigger functions and procedures.
- `name%ROWTYPE` and `schema.name%ROWTYPE` over a relation that does not
  exist are 42P01 `relation "name" does not exist`, with no position and
  the CONTEXT `compilation of PL/pgSQL function "f" near line N`, where N
  is the variable's line.
- Declarations are checked in the order PL/pgSQL compiles them, so the
  first missing type is the one reported. `CREATE OR REPLACE` that is
  refused keeps the routine it would have replaced. `SET
  check_function_bodies = off` skips the check, as `plpgsql_validator`
  does.
- Every type a declaration may name still resolves: built-in types with
  modifiers and arrays, domains, enums, composites, a table's, a view's, a
  temporary table's or a catalog's row type (bare, qualified or
  `%ROWTYPE`), `record`, `refcursor`, and a `%TYPE` of a variable or
  column.

Fixes RP-573 (tests/backlog/test_rp_found_rp566.py::test_plpgsql_declare_of_an_undefined_type_is_refused,
tests/backlog/test_rp_found_rp566.py::test_plpgsql_declare_type_lookups,
tests/backlog/test_rp_found_rp566.py::test_plpgsql_declare_check_follows_check_function_bodies).
Files RP-617 (a declared text that is not one type name, or a `%TYPE`
nothing resolves), RP-618 (a variable of a pseudo-type), RP-619 (a missing
type reported after a later syntax error), RP-620 (`pg_temp` in a session
with no temporary schema) and RP-621 (a PL/pgSQL compile error in CREATE
FUNCTION has no position).
