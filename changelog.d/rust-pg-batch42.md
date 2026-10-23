### Rust PostgreSQL server: the built-in text-search configurations can be changed

A built-in text-search configuration (`english`, `danish`, ...) is an
ordinary row `initdb` made, owned by the bootstrap superuser. A superuser
may now rename it, remap it, comment on it or drop it, as on PostgreSQL
15.19.

#### Fixed

- Fixes RP-165
  (`tests/backlog/test_rp_batch34.py::test_scenario[a built-in configuration can be altered]`,
  `tests/backlog/test_rp_batch42.py::test_scenario[a built-in configuration's map and comment]`,
  `[a built-in configuration dropped]` and
  `[a built-in configuration is the bootstrap superuser's]`): every change
  to a built-in configuration was 0A000.
  - **Changed.** The database keeps its own copy of the row, under the
    built-in oid, and the copy replaces the dumped row in `pg_ts_config`,
    `pg_ts_config_map`, `regconfig` and `to_tsvector`.
  - **Dropped.** It leaves a tombstone row behind.
  - **Transactions.** Both roll back with the transaction.
  - **Permission.** A role that is not a superuser is refused as the
    owner's non-member is.
- Fixes RP-190 (`tests/backlog/test_rp_batch42.py::test_scenario[a call of no function beside an aggregate]`):
  `select nosuch(1), count(*) from t` was 0A000 `this target is not
  supported yet`. It is now PostgreSQL's 42883 `function nosuch(integer)
  does not exist`.
