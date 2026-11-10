### Rust PG server: a PL/pgSQL variable of a composite type is a record of that type

A variable declared of a table's row type, a `CREATE TYPE ... AS (...)`
composite or a relation's `%ROWTYPE` -- `declare r t;` -- is now compiled
as PostgreSQL compiles it: a record of that type, whose fields the body can
read and assign.

#### Fixed

- `declare r t; begin r.a := 5;` was refused at CREATE FUNCTION (and in a
  DO block) with 42601 `"r.a" is not a known variable`: libpg_query's
  PL/pgSQL grammar has no catalog and made every declared type but
  `record` a scalar. The server now looks each declaration's type up
  before the body is parsed, and one the catalog says is composite (a
  table, a composite type, `rel%ROWTYPE`, schema-qualified or not) is
  compiled as a record of it.
- Such a record is NULL until assigned, and each field of it reads NULL of
  the field's type; assigning a field first makes it a row of NULL fields
  (`instantiate_empty_record_variable`); the assignment checks the field's
  domain and length (`r.a := -5` is the domain's 23514, an overlong
  `varchar(3)` 22001).
- A row moved into it by `SELECT ... INTO`, `FOR ... IN` or `FETCH` is
  converted to its fields by position, by the assignment rule; a missing
  value is NULL and an extra one ignored. Before, `select * into r` kept
  only the first column and `raise '%', r` was 0A000 `a cast of int32 to
  t687 is not supported yet`.
- `pg_typeof(r)` names the row type, as PostgreSQL does.

Fixes RP-689 (tests/backlog/test_rp_found_rp670.py::test_row_variable_fields_are_assignable;
more cases in tests/backlog/test_rp_found_rp689.py::test_a_variable_of_a_row_type_is_a_record_of_it
and ::test_a_function_compiles_a_row_typed_variable).

Filed while working it: RP-837 (reading a field a record lacks is 42P01,
not 42703) and RP-838 (a column a record variable's field shares is not
refused as ambiguous).
