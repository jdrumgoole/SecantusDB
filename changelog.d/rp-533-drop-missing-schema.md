### A DROP in a schema that does not exist names the schema

`drop table nosch.t` -- and the same for a view, materialized view, index or
sequence -- now fails as PostgreSQL's `DropErrorMsgNonExistent` does, with
3F000 `schema "nosch" does not exist`, and under IF EXISTS skips with the
notice `schema "nosch" does not exist, skipping`. The server used to report
the relation itself as missing. A quoted `"nosch.t"` is still one name in
the search path, reported as a missing table.

#### Fixed

- Rust PG server: a DROP of a relation in a missing schema is 3F000, or its
  schema's skipping notice under IF EXISTS.
  Fixes RP-533 (tests/backlog/test_rp_found_rp528.py::test_drop_in_a_missing_schema)
