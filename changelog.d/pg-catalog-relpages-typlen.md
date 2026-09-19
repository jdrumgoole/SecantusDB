### `pg_class.relpages` and `pg_type.typlen` did not exist

Two catalog columns a JDBC client reads were simply absent, and each failed a
query outright rather than returning a wrong value.

`pg_class.relpages` is what pgjdbc's `getIndexInfo` selects as `PAGES`, so its
absence failed that whole query with `column "relpages" does not exist` before
any row came back. `pg_type.typlen` is read by `getMaxNameLength()`, which
selects it for `typname = 'name'` and treats a missing row as fatal — and
`name` is not a type this server stores, so the row had to be added for the
lookup to resolve at all.

All values measured against PostgreSQL 14 on 2026-09-19.

#### Added

- `pg_class.relpages`. A fresh table reports 0; a fresh **index** reports 1,
  because an index has its metapage from the moment it exists — 0 would be the
  obvious guess and is wrong.
- `pg_type.typlen`, for built-in, array, composite, range, domain and enum
  rows. An enum is a fixed 4-byte oid reference; everything else user-defined
  is varlena (-1). A domain inherits its base type's width. TypeInfoCache
  filters its array lookup on `typlen = -1`, so the array rows' value is
  load-bearing rather than decorative.
- a `pg_type` row for `name` (oid 19), a type this server never stores but a
  client still resolves by name.

#### Fixed

- A comment added with the declared-char-type change claimed this catalog
  emits no `_<type>` array rows for any type. That was inferred from
  `PG_TYPENAME` holding no `_` names and is **false** — array rows are
  synthesised from `typarray`, and `_text` (1009) has always been served.

Measured on pgjdbc's `DatabaseMetaDataTest` (194 tests): **36 failures → 35**,
23 distinct → 22, no regressions. `getClientInfoProperties` goes green.
`relpages` also unblocks `getIndexInfo` far enough to reach the *next* blocker
in the same query, now filed in `tasks/backlog.md` with both of its halves.
