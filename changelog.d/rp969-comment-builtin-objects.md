### COMMENT ON a built-in type, operator or access method replaces initdb's description

A comment on a built-in object now lands where PostgreSQL puts it: in `pg_description`, keyed by the object's oid, replacing the description initdb wrote. `COMMENT ON TYPE int4` used to be accepted and then lost behind initdb's text, and `COMMENT ON OPERATOR` and `COMMENT ON ACCESS METHOD` were refused outright. `IS NULL` removes the description, initdb's included, and a missing operator, operand type or access method answers with PostgreSQL's own error.

#### Fixed

- Rust PG server: `COMMENT ON TYPE` over a built-in type, `COMMENT ON OPERATOR` and `COMMENT ON ACCESS METHOD` replace (or, with `IS NULL`, delete) initdb's description, as `obj_description` and `pg_description` show.
  Fixes RP-969 in part (tests/backlog/test_rp_found_rp514.py::test_comment_on_builtin_object_replaces_initdb_description)
