### psycopg can read composite types back (`CompositeInfo.fetch`)

Registering a composite type with psycopg — `CompositeInfo.fetch(conn, name)` —
now returns its fields against the Rust PostgreSQL server, matching a real
PostgreSQL server exactly. The query psycopg emits is the most demanding
catalog read the server has faced: `pg_type LEFT JOIN (SELECT
array_agg(attname), array_agg(atttypid) FROM (pg_attribute JOIN pg_type) GROUP
BY attrelid)`, with a `coalesce(..., '{}')` around each aggregate column. It
combines four things the server could not previously do at once — a JOIN whose
side is a subquery materialised from an aggregate, an `oid[]` array column, a
`coalesce` projected as an output column, and a base type surviving the LEFT
JOIN with no matching fields.

Three of those were silent wrong answers rather than errors. `array_agg(atttypid)`
came back tagged as text, so psycopg read the field-type list as the raw string
`"{23,25}"` instead of a list of oids. A base type with no fields (or any type
whose composite subquery found nothing) dropped out of the result entirely, or
returned `None` for `field_names`/`field_types` where PostgreSQL returns two
empty arrays — because the `coalesce` fallback never ran on a LEFT-JOIN miss,
which is the one case it exists for. And a composite field whose type is itself
a user type (`CREATE TYPE t AS (sub other_composite)`) was silently dropped from
`pg_attribute`, since the field-type-to-oid lookup consulted only builtin types.

#### Added

- `CompositeInfo.fetch` is supported: the aggregate-subquery join side, the
  `oid[]` (`_oid`, 1028) array column type, the coalesce-as-output-column, and
  the coalesce-fills-`'{}'`-as-an-empty-array-on-a-LEFT-JOIN-miss all work
  together.

#### Fixed

- `array_agg(atttypid)` (and any `oid[]` column) carries the `_oid` array oid
  instead of falling through to `varchar`, so a client parses it as a list of
  oids rather than a string.
- A composite field whose type is itself a composite, enum, or range type
  appears in `pg_attribute` with that type's own oid, instead of being dropped.
- A `coalesce(col, '{}')` over a LEFT-JOIN miss yields an empty array, not
  `NULL` and not the text `"{}"`.
