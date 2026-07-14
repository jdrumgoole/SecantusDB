### SQL: CREATE SCHEMA and schema-qualified user types

`CREATE SCHEMA [IF NOT EXISTS]` / `DROP SCHEMA [IF EXISTS] [CASCADE]` land,
with user-declared types (enum / domain / composite) creatable and droppable
under a schema (`CREATE TYPE testschema.testcomp AS (…)`). Qualified names
resolve everywhere psycopg's type machinery needs them: `to_regtype`, the
`'schema.name'::regtype` literal cast (previously an internal error — the
pushdown's cast coercion knew `regclass` but not `regtype`), `oid::regtype`
rendering, and `TypeInfo`/`CompositeInfo` fetches by dotted string or
`sql.Identifier` spelling. `pg_namespace` carries user schemas with minted
oids and `pg_type` reports the bare `typname` under the schema's
`typnamespace`. Dropping a non-empty schema without CASCADE is a 2BP01
dependency error, CASCADE drops the contained types, and `DROP TYPE IF
EXISTS` tolerates a missing schema. This clears the psycopg gauge's entire
"CREATE SCHEMA is not supported" cluster and unblocks the schema-gated
composite/range/typeinfo fixtures. (Schema-qualified *tables* remain 0A000 —
`tasks/backlog.md`; user-defined `CREATE TYPE … AS RANGE` likewise.)

#### Added

- `catalog.py`: schema registry (`create_schema` / `schema_exists` /
  `drop_schema` / `list_schemas`); `engine.py`: `CREATE`/`DROP SCHEMA`
  routing, qualified-name extraction for `CREATE`/`DROP TYPE`;
  `virtual.py`: user-schema `pg_namespace` rows, dotted-name splitting in
  `pg_type`, quote-normalized qualified lookups; `planner.py`: the
  `::regtype` literal cast resolves built-ins and user types (42704 on
  unknown, like PG).
