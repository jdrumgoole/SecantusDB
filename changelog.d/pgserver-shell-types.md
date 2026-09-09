### Shell types and base types on the Rust PostgreSQL server

`CREATE TYPE "a-b";` used to be refused outright (`DefineStmt is not supported
yet`), which closed the door on the sequence psycopg's own suite uses to build
a custom base type: a shell type, two `LANGUAGE internal` I/O functions that
name it, and the full `CREATE TYPE "a-b" (input=invin, output=invout,
like=text)` that completes it. The Rust server now runs that sequence the way
PostgreSQL 16 does. A shell is a `pg_type` row with no array type that nothing
can be cast to (`type "a-b" is only a shell`), `CREATE FUNCTION ... LANGUAGE
internal` registers the wrapper with PostgreSQL's notices for a shell argument
or return type (and creates a shell for an unknown return type), and the full
`CREATE TYPE` checks its shell and both I/O functions — existence, exact
signature, return type — with PostgreSQL's messages before completing the type.

A completed type carries its values as text: `'hello-inv'::"a-b"` and
`'{hello-inv}'::"a-b"[]` come back described with the type's own oid and its
`typarray`, `TypeInfo.fetch` finds both, and a name that needs quoting (`€`,
`order`, `foo bar`, `FooBar`) renders quoted through `regtype`. `DROP TYPE`
and `DROP FUNCTION` know the dependency between a type and its I/O functions:
RESTRICT refuses with `2BP01` and PostgreSQL's per-dependent DETAIL lines,
CASCADE drops them with the `drop cascades to ...` notice, and both roll back
inside a transaction. `DROP TYPE IF EXISTS` on a missing type is now the
`does not exist, skipping` notice rather than silence.

#### Added

- `secantus-pgplan`: plans `DefineStmt` of kind TYPE (shell and full forms),
  `CreateFunctionStmt` (`LANGUAGE internal` only) and `DROP FUNCTION`;
  `DropType` carries `CASCADE`; a base type registry beside the enum /
  composite / range ones, with a defined type casting text to itself and a
  shell refused as `is only a shell`.
- `secantus-pgserver`: the `__sql_base_types__` catalog (oid band 71000,
  `typarray` = oid + 100000), `CREATE FUNCTION` rows in the shared
  `__sql_functions__` shape, `pg_type` rows for shells and base types, and
  the dependency-aware `DROP TYPE` / `DROP FUNCTION` with PostgreSQL 16's
  errors and notices. A table column typed as a shell is refused.
- psycopg gauge: `test_sql.py::TestLiteral::test_invalid_name` passes for all
  five spellings.
