### Concurrent sessions get their own temp-table namespaces

Postgres gives every backend a private `pg_temp_<n>` schema, so two open
connections can each `CREATE TEMPORARY TABLE bar` without colliding.
SecantusDB's SQL server shared one namespace: the second concurrent create
failed with `42P07 relation "bar" already exists`, a real divergence that
connection-pooled applications and driver test suites hit immediately.

Each session now allocates its own `pg_temp_<n>` namespace the first time it
creates a temp table. Unqualified names resolve against the session's temp
namespace ahead of `public` — so a temp table shadows a permanent one of the
same name, exactly like real Postgres — and an explicit `pg_temp.<name>`
qualifier resolves to the session's own namespace (`CREATE TABLE pg_temp.t`
creates a temp table, and `CREATE TEMP TABLE` aimed at any other schema is
rejected with `42P16`). COPY and extended-protocol Describe resolve through
the same path, temp-table SERIAL sequences are per-session too, and
`pg_class` / `information_schema.tables` report the bare relation name under
its session's temp schema.

#### Fixed

- `sql/session.py`: per-session `pg_temp_<n>` namespace, allocated lazily on
  first temp-table creation (pid-seeded so a crashed process's stale entries
  can't collide with a new one's).
- `sql/planner.py`: `qualify_from_search_path` resolves the session temp
  namespace first (unless `pg_temp` is placed explicitly on `search_path`),
  rewrites `pg_temp.<name>` to the session's namespace, and
  `qualify_temp_create_target` homes `CREATE TEMP TABLE` targets there;
  `pg_table_is_visible` lowers against bare relnames.
- `sql/engine.py`: `copy_plan` and extended-protocol Describe apply the same
  search-path / temp-namespace resolution as execution.
- `sql/executor.py`: duplicate temp-table errors name the bare relation
  (`relation "bar" already exists`); error diagnostics report the session's
  actual `pg_temp_<n>` schema.
