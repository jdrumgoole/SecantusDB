### A relation named in another database, or another session's temp schema, is answered as PostgreSQL does

A table reference qualified by a database other than the session's —
`select * from x.public.t` — is now refused with PostgreSQL's 0A000
`cross-database references are not implemented: "x.public.t"` before any
lookup, where the Rust PostgreSQL server used to drop the database name and
look the table up. And a missing relation in another session's temporary
schema, `select * from pg_temp_1.nosuch`, is reported as
`relation "pg_temp_1.nosuch" does not exist` at the qualifier, keeping the
schema it was written with.

#### Fixed

- A three-part relation name whose database is not the session's is 0A000,
  as `RangeVarGetRelidExtended` answers it.
  Fixes RP-964 (tests/backlog/test_rp_qualified_calls.py::test_other_database_pg_temp_is_a_cross_database_reference)
- A relation in a `pg_temp_N` other than the session's is keyed and named
  in that schema.
  Fixes RP-965 (tests/backlog/test_rp_qualified_calls.py::test_other_sessions_temp_schema_relation_is_named_as_written)
