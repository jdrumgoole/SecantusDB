### An unmade `pg_temp` is no schema to a routine's types

PostgreSQL finds `pg_temp` only once the session has made its temporary
schema, so a CREATE FUNCTION whose parameter or DECLARE names
`pg_temp.sometype` before then fails with 3F000 `schema "pg_temp" does not
exist`. The Rust PostgreSQL server now does the same, instead of reporting
a missing type.

#### Fixed

- `routine_type_missing` checks `pg_temp` against the session's temporary
  schema being made (`LookupExplicitNamespace`).
  Fixes RP-620 (tests/backlog/test_rp_found_rp573.py::test_pg_temp_type_in_a_session_with_no_temp_schema)
