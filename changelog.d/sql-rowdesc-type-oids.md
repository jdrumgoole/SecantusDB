### SQL server: RowDescription reports real Postgres type OIDs for computed columns

A libpq client keys its result decoding off the type OID in each
`RowDescription` column, and SecantusDB's SQL server used to fall back to
`text` (25) for most computed results — `CASE` expressions, `array[...]`
constructors, array casts, integer arithmetic, bound parameters — and widened
`smallint`/`real` to `integer`/`double precision` everywhere. The first
external-gauge run (psycopg 3's own test suite plus the sqllogictest corpus,
see `tasks/sql-gauges-plan.md` §6) flagged this as the single
highest-leverage divergence. Computed and derived columns now describe with
the OID real Postgres would use, so typed loaders in psycopg / pg8000 /
SQLAlchemy decode results without special-casing.

#### Added

- `pg_typeof()` and `'name'::regtype`: the type-introspection pair psycopg's
  type suite leans on (`select pg_typeof(%s::int2) = 'smallint'::regtype`).
  `pg_typeof` resolves at plan time from the same static inference that types
  RowDescription; `::regtype` normalizes any accepted spelling (`int4`,
  `varchar`, `float4`) to the canonical pretty form `pg_typeof` prints.
- `typemap.py`: first-class `int2` (21) and `float4` (700) type tags —
  `smallint` / `real` columns, casts, arrays (`1005` / `1021`), catalog
  `pg_type` rows, and `information_schema` spellings; `SMALLSERIAL` columns
  now describe as `int2` instead of `text`.

#### Fixed

- `planner.py`: type inference for computed SELECT columns — `CASE` types
  from its result branches; `array[...]` and array casts report the array
  OID; integer arithmetic stays integer (`int + int` → `int4`, matching
  `_pg_div`'s truncating division) instead of `numeric`; an unadorned
  decimal constant (`SELECT 1.5`) is `numeric`, matching Postgres;
  `sum(int2/int4)` → `int8`, `sum(int8)` → `numeric`, `avg(integer)` →
  `numeric` per Postgres' aggregate result types; `CAST($1 AS SMALLINT)`
  coerces its text-bound value numerically.
- `pgextended.py`: `SELECT $1` describes with the parameter OID the client
  declared in Parse (psycopg binds a small Python int as `int2`), instead of
  re-inferring from the substituted Python value.
- `pgextended.py`: binary result format and binary parameters now cover
  arrays (the real ndim/hasnull/elemoid wire layout, both directions). The
  correct array OIDs engage a libpq client's binary array parser, which the
  text-bytes fallback would have fed garbage.
