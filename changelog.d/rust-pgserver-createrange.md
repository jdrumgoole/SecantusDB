### Rust pgserver: CREATE TYPE AS RANGE

Custom range types work now: `CREATE TYPE name AS RANGE (subtype = int4)`
registers the type in the catalog (a `__sql_ranges__` collection paralleling
the enum and composite catalogs), and a value casts, renders, and compares
using the subtype's own machinery — `'[1,5)'::myrange`. psycopg's
`RangeInfo.fetch` resolves the subtype through the existing `pg_type` /
`pg_range` join, and `DROP TYPE` removes it. Unlike a builtin `int4range`, a
custom range with no canonical function is not auto-canonicalised, matching
PostgreSQL (`'[1,4]'` stays `[1,4]`).

#### Added
- `CREATE TYPE … AS RANGE (subtype = …)` DDL, catalog, and `DROP TYPE`.
- Casts, rendering, and comparison for custom range types over int/numeric/
  date/timestamp subtypes; `RangeInfo.fetch` support via `pg_range`.
