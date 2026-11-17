### A type made in `pg_temp` lives in the session's temp schema

`CREATE TYPE pg_temp.t` -- a composite, an enum or a range -- now makes the
type in the session's own temporary schema, making that schema as a
temporary table does, exactly as PostgreSQL 15 does. The type resolves by
its bare name, `DROP TYPE` reaches it, other sessions cannot name it, and
it is dropped when the session ends. Before, a composite or range landed
in `public` and an enum in a schema literally called `pg_temp`, and all
three outlived the session.

#### Fixed

- `CREATE TYPE pg_temp.t` (composite, enum, range) makes the type in the
  session's `pg_temp_N`; `DROP TYPE` of it by `pg_temp.t` or bare name
  works, and the session's temp types are dropped at disconnect (and any a
  killed server left, at startup).
- A schema-qualified range type and its multirange are listed in
  `pg_type` under their schema's namespace, not `public`.

Fixes RP-971 (tests/backlog/test_rp_qualified_calls.py::test_standalone_temp_composite_is_in_the_temp_schema, tests/test_rust_pgserver_slice.py::test_pg_temp_types_live_in_the_temp_namespace_and_go_with_the_session)
