### A missing qualified relation is named and pointed at as written

`select * from pg_temp.nosuch` now fails with `relation "pg_temp.nosuch"
does not exist`, the name the statement wrote, rather than the session's
internal `pg_temp_N` namespace. Every missing schema-qualified relation is
now positioned at its qualifier, as PostgreSQL points at a range
reference, instead of at its bare name.

#### Fixed

- A missing `pg_temp.x` relation is reported as `pg_temp.x`, and a missing
  `schema.x` (or `db.schema.x`) at the start of the reference; a
  `schema.x.col` column reference earlier in the statement is not taken
  for it.
  Fixes RP-882 (tests/backlog/test_rp_qualified_calls.py::test_pg_temp_relation_is_named_as_written, tests/backlog/test_rp_qualified_calls.py::test_qualified_missing_relation_points_at_its_qualifier)
