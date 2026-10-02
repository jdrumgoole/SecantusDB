### Rust PostgreSQL server: a privilege bypass, has_table_privilege, and backlog leftovers

#### Security

- A table named with its schema (`s.t`) skipped the privilege check entirely: any role could SELECT and INSERT on it. The check now resolves the qualified relation.
- `has_table_privilege` answered true for every role. It now answers from grants, owners, role membership and PUBLIC.

#### Fixed

- Two cases accepted duplicate rows silently on a dotted column: `ALTER TABLE ADD UNIQUE` over existing rows, and `UNIQUE NULLS NOT DISTINCT`. Both now reject them.
- Binary input of `xid`, `cid`, `xid8`, `txid_snapshot`, `pg_snapshot` and their arrays was stored as raw bytes. It is now decoded.
- A `pg_temp` function called without its `pg_temp.` prefix is refused (42883). Leftovers from a killed server are dropped at open.
- `min` / `max` over types with no such aggregate in PostgreSQL 15 are refused.
- SET of server-start or reload-only settings is refused with 55P02.
- Index comments are kept per schema.
- `pg_get_viewdef` qualifies other schemas' tables.
- `'t'::regclass` and `to_regclass` walk `search_path`.
- An aggregate `FILTER` containing a subquery works for `array_agg`, `json_agg`, `jsonb_agg`, `json_object_agg` and window aggregates.
- The SQLAlchemy gauge runner deletes its temp directory.
