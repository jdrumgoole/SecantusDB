### pg_type grows real array-type rows

Every type that advertises a `typarray` now has the paired array-type row
in `pg_catalog.pg_type` — `_int4` with `typelem = 23` and friends, for
built-ins, enums, domains, composites and table row types — where before
the advertised oid resolved to nothing. The `typelem` column exists at
all now, `'pg_catalog.array_in'::regproc` strips the schema the way
PostgreSQL renders search-path-visible functions (so pgjdbc's standard
is-array probe matches), and `pg_class` carries a `relacl` column (null,
single-user server). pgjdbc's EnumTest enum-array resolution now works;
psycopg's `TypeInfo.fetch` finds array types by oid.

#### Added
- Array-type rows in `pg_type` (typname `_<elem>`, `typelem`,
  `typinput = array_in`) for every type with a `typarray`.
- `pg_type.typelem`, `pg_class.relacl` columns; `::regproc` casts.
