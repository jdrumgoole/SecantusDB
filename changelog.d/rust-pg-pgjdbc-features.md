### Rust PostgreSQL server: money, large objects, refcursors and database settings

The Rust PostgreSQL server gained the PostgreSQL 15 features pgjdbc's suite was missing. pgjdbc now runs 7,354 tests, up from 5,637, because three classes no longer fail in setup. 84 fail, down from 133. pgx stays at 377 passed / 0 failed. Each feature is pinned by a new corpus checked against PostgreSQL 15.

#### Added

- The `money` type, transcribed from `cash.c`. It is stored as Decimal128, as on the Python server.
- Large objects:
  - every `lo_*` function, from SQL and over the Fastpath function-call protocol;
  - stored in the Python server's collections;
  - `pg_largeobject_metadata`.
- PL/pgSQL `OPEN` for refcursors: `FOR query`, `FOR EXECUTE` and bound cursors. `refcursor` is now a type.
- `ALTER DATABASE ... SET` / `RESET`, applied at connect, and a `pg_settings` view.
- `SET LOCAL` and `set_config(..., true)` are undone at transaction end. A `SET` inside a rolled-back block is undone. ParameterStatus is re-sent when a value changes back.
- `information_schema._pg_expandarray`, and `unnest` over `int2vector` / `oidvector`.
- Named protocol portals appear in `pg_cursors`.
- Smaller additions:
  - `getdatabaseencoding` and `pg_encoding_to_char` / `pg_char_to_encoding`;
  - negative numeric scale;
  - every DateStyle keyword;
  - `interval + datetime`;
  - timestamp-to-time assignment casts;
  - `inet` / `cidr` / `bit` / `varbit` in `pg_type`;
  - `INSERT ... SELECT` with an uncorrelated subquery.
