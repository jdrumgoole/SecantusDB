### Rust PG server: a routine's `%TYPE` parameter or result has the column's type

A CREATE FUNCTION or CREATE PROCEDURE parameter, or a RETURNS clause, written
as `t.v%type` was stored as a type literally named `"t.v"`. A call then saw a
`record`, `pg_get_function_arguments` printed `x "t.v"`, and a function
declared `returns t.n%type` returned NULL instead of its value. A LANGUAGE sql
routine's `%TYPE` parameter was refused with 3F000 `schema "t" does not
exist`. A reference to a column or relation that does not exist was accepted
silently.

The server now resolves each one as PostgreSQL 15.19's `LookupTypeNameExtended`
does. The parameters are resolved in order, then the result.

- The reference is `relation.column`, `schema.relation.column` or
  `database.schema.relation.column`. The column can be any attribute,
  including a system column such as `ctid`.
- The type is the column's type without its modifier. A `varchar(3)` column
  gives `character varying`. A domain or enum column gives the domain or the
  enum.
- Each reference sends the NOTICE `type reference t.v%TYPE converted to
  character varying`. It is sent before any error a later parameter raises.
  RETURNS TABLE with one column looks that column's type up a second time,
  as PostgreSQL does.
- A reference that resolves to nothing raises PostgreSQL's error, with no
  position: 0A000 for another database, 42601 for more than four names,
  3F000 for a missing schema (`pg_temp` before the session has a temporary
  table), 42P01 for a missing relation, and 42703 `column "c" of relation
  "t" does not exist`.

#### Fixed

- `secantus-pgplan` (`plpgsql_pct_type.rs`, `lib.rs`, `schemas.rs`):
  `routine_percent_type` resolves a signature's `%TYPE`, and
  `resolve_routine_types` applies it in CREATE FUNCTION's lookup order.
  Fixes RP-822 (tests/backlog/test_rp_found_rp818.py::test_a_routines_percent_type_parameter_is_resolved).
- A PL/pgSQL DECLARE of `pg_catalog.pg_class.relname%type` (RP-818's path)
  was refused with 42601. It shares the new relation lookup, which finds a
  catalog relation qualified by `pg_catalog`.

#### Found while working it

- RP-830: a DROP or ALTER FUNCTION signature's `%TYPE` is not resolved.
  `drop function f(t.v%type)` is 42704 `type "t.v" does not exist`.
