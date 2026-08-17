### getCatalogs lists the postgres maintenance database

`pg_database` now reports the connected database plus `postgres` — the
maintenance database every real PG cluster carries and the one JDBC
clients enumerate through. pgjdbc's `getCatalogs` asserts both are
present and sorted. MongoDB-wire namespace names (e.g. `local`) are
deliberately kept out of the PG catalog — a PG client must never see
them as a connectable catalog.

#### Fixed
- `pg_database` / `getCatalogs` includes `postgres` (deduped when the
  connection is already to `postgres`).
