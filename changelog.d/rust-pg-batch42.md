### Rust PostgreSQL server: durable commits, schema and object privileges, snapshot functions

#### Changed

- In durable mode (the default), every acknowledged COMMIT on `secantusd-pg` is now synced to disk. Before, a commit acknowledged just before a process kill was lost: 20 of 20 times in a kill test, against 0 of 20 now. This matches PostgreSQL's default. Durable autocommit writes cost about 10x more. Test fast-storage mode (`SECANTUS_TEST_FAST_STORAGE=1`) is unchanged. The Rust MongoDB server is unaffected.

#### Fixed

- Schema USAGE is enforced: reading `s.t` without it is `42501`.
- GRANT / REVOKE on schemas, sequences and functions is recorded.
- `has_schema_privilege`, `has_sequence_privilege`, `has_function_privilege` and `has_column_privilege` answer from grants and owners, not true for everyone.
- `set_config(x, v, true)` outside a block lasts only for its statement.
- Large-object descriptors opened in autocommit close at statement end.
- Other sessions' temp tables are listed in `pg_class`.
- A notice raised before `pg_sleep` is sent during the sleep, and the sleep can be cancelled.

#### Added

- `information_schema.role_table_grants` and `table_privileges`.
- The snapshot functions: `pg_snapshot_xmin` / `xmax` / `xip`, `pg_visible_in_snapshot`, `pg_current_snapshot`, and their `txid_` forms.
