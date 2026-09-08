### Schema-qualified composite types are distinct types

`CREATE TYPE testschema.testcomp AS (...)` on the Rust PostgreSQL server now
creates a type that is genuinely distinct from a bare `testcomp` in `public`,
matching PostgreSQL. Previously a qualified type name was resolved to its last
part, so `testschema.testcomp` collided with `testcomp` and the second `CREATE`
failed with `42710 type already exists`.

This mattered far beyond one CREATE: psycopg's composite-type test fixture is
session-scoped and creates `testschema.testcomp` beside a bare `testcomp` in one
script, so the collision failed the fixture and cascaded to every composite
test. With the two types now coexisting, `CompositeInfo.fetch` resolves each
name to its own type — bare `testcomp`, `testschema.testcomp`, and the quoted
`"testschema"."testcomp"` form a `sql.Identifier` renders all reach the right
oid, while `typname` stays unqualified as PostgreSQL keeps it. `DROP TYPE
testschema.testcomp` is schema-aware and leaves the bare type standing.

#### Added

- Schema-qualified composite type names (`CREATE TYPE schema.name AS (...)`) are
  stored and resolved per `(schema, name)`; a bare name and a schema-qualified
  one with the same last part are distinct types.
- `to_regtype` resolves a schema-qualified type reference in bare,
  `schema.name`, and quoted `"schema"."name"` forms; a bare name resolves only
  the `public` type (matching the default search_path).
- `DROP TYPE schema.name` targets the schema-qualified type; `public.name`
  normalises to the bare name.
