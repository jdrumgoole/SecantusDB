### Rust PG server: an unqualified CREATE TYPE or CREATE DOMAIN lands in the search path's first schema

After `set search_path = s`, a bare `create domain`, `create type ... as
(...)`, `as enum`, `as range` or a shell `create type` now makes the type
in `s`, as PostgreSQL's `QualifiedNameGetCreationNamespace` does. Before,
each went to `public` whatever the path, so `create table t (d d)` under
the same path then failed with 42704. A bare type name in `DROP TYPE`,
`DROP DOMAIN`, `ALTER DOMAIN`, `ALTER TYPE` (`ADD VALUE`, `RENAME`,
`OWNER TO`, `SET SCHEMA`) and `COMMENT ON TYPE` / `DOMAIN` is now found
through the path too, so a type made that way can be altered and dropped
by the name it was created with. A path with no usable schema is 3F000
`no schema has been selected to create in`.

#### Fixed

- `secantus-pgplan`: an unqualified `CREATE TYPE` / `CREATE DOMAIN` takes
  `schemas::creation_schema()`, and `resolve_type_names` qualifies the bare
  type a `DROP` / `ALTER` / `COMMENT` statement names through the path.
  Fixes RP-1085 (tests/backlog/test_rp_found_rp1048.py::test_type_created_in_search_path_schema,
  tests/backlog/test_rp_found_rp1048.py::test_search_path_type_round_trip).
- Filed RP-1218: `ALTER DOMAIN s.d RENAME TO e` on a domain outside `public`
  is a silent no-op (tests/backlog/test_rp_found_rp1085.py::test_rename_non_public_domain).
- Filed RP-1219: a bare type name in `DROP TYPE` reaches `public` when the
  path does not (tests/backlog/test_rp_found_rp1085.py::test_bare_drop_type_misses_public_off_path).
- Filed RP-1220: a non-public schema's types do not share one name space
  across kinds (tests/backlog/test_rp_found_rp1085.py::test_type_name_unique_across_kinds_in_schema).
- Filed RP-1222: a `pg_catalog` written first on the path is not the
  creation namespace (tests/backlog/test_rp_found_rp1085.py::test_explicit_pg_catalog_is_creation_namespace).
