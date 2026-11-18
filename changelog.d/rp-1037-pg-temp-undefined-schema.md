### `pg_temp` is no schema until the session makes one

Before a session has created a temporary object, PostgreSQL has no temp
namespace for it, so a name qualified with `pg_temp` names a schema that
does not exist. `'pg_temp.zz'::regclass` and `'pg_temp.zz'::regtype` now
answer 3F000 `schema "pg_temp" does not exist` there, as PostgreSQL does,
instead of reporting the relation or type as missing.

#### Fixed

- `secantusd-pg`: an explicit `pg_temp` qualifier on a regclass or regtype
  input is looked up as `LookupExplicitNamespace` looks it up.
  Fixes RP-1037 (tests/backlog/test_rp_qualified_calls.py::test_pg_temp_name_without_a_temp_namespace_is_an_undefined_schema)
