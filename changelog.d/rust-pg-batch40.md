### Rust PostgreSQL server: system types, all PG 15 settings, session-private pg_temp functions

pgjdbc against the Rust PostgreSQL server went from 22 failures to 3 (7,354 tests). The remaining three are an UPDATE of `pg_class`, a `LANGUAGE C` function, and a client-socket check. pgx stays at 377 passed / 0 failed, including two runs over one store. Every fix is checked against PostgreSQL 15 and pinned in the `b40_fixes` and `b40_types` corpora.

#### Added

- The `macaddr`, `macaddr8`, `pg_lsn`, `txid_snapshot`, `pg_snapshot`, `xid`, `xid8` and `cid` types.
- All 345 PostgreSQL 15 settings are SHOW-able with PG's defaults. `pg_settings` carries their metadata.
- Standalone composite types appear in `pg_class`.

#### Fixed

- `pg_temp` functions are private to their session and dropped at disconnect.
- `ADD PRIMARY KEY USING INDEX` keeps the index's name and its INCLUDE columns.
- A LIKE pattern ending in its escape character raises 22025 only when matching reaches it.
- `_custom` array type names follow PostgreSQL's rule. Columns of composite or enum array types appear in `pg_attribute`.
- Two-dimensional enum and composite arrays render correctly.
- A `BC` date keeps a UTC offset written after it.
- Describe of a statement reports text formats. The time-zone name is case-canonical.
- System relations carry initdb's `relacl`.
- Startup ParameterStatus no longer sends `search_path`.
- The pgjdbc runner gives its `test` role a password, as pgjdbc's CI does.
