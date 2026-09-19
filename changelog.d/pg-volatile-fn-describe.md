### Prepared `pg_sleep` / `pg_notify` / advisory-lock / `lo_creat` calls keep working

On the Python PostgreSQL server, `select 1, pg_sleep(0.25)` failed on its
seventh run with 0A000 "cached plan must not change result type". psycopg
prepares a statement after five runs. After that, each Bind compares the type
Describe reported with the type Execute produced, and for these functions the
two were different.

Describe cannot run a volatile function, so it reads the type from a table.
Execute types the call through the planner, which fell back to `text`. The two
disagreed for `pg_sleep`, `pg_notify`, the blocking `pg_advisory_lock*` forms,
`pg_advisory_unlock_all`, `lo_creat` and `lo_create`. Both now report what
PostgreSQL reports: `void` for the first four, `oid` for the large-object
creators.

#### Fixed

- `sql/planner.py`: return types for these functions match `pg_proc`.
- `sql/engine.py`: the Describe table uses `void` for `pg_sleep` and the
  advisory locks, and now covers the `_shared` / `_xact` / `try` variants.

#### Testing

- `tests/test_pg_volatile_fn_describe.py`: each function prepared and repeated,
  asserting PostgreSQL's oid on every run, plus the original
  `select 1, pg_sleep(0)` shape under psycopg's default prepare threshold.
