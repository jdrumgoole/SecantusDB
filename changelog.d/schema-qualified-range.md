### Schema-qualified RANGE and ENUM types are distinct types

`CREATE TYPE testschema.testrange AS RANGE (...)` on the Rust PostgreSQL server
now creates a type genuinely distinct from a bare `testrange` in `public`,
matching PostgreSQL. Previously a schema-qualified `CREATE TYPE ... AS RANGE`
(and `AS ENUM`) resolved to its last name part, so `testschema.testrange`
collided with `testrange` and the second `CREATE` failed `42710 type already
exists`.

This mattered beyond one CREATE: psycopg's session-scoped range fixture creates
`testschema.testrange` beside a bare `testrange` in one script, so the collision
failed the fixture and cascaded to every range test. With the two types now
coexisting, `RangeInfo.fetch` resolves each name to its own type — bare
`testrange`, `testschema.testrange`, and the quoted `"testschema"."testrange"`
form a `sql.Identifier` renders all reach the right oid, each carrying its own
subtype, while `typname` stays unqualified. `DROP TYPE testschema.testrange` is
schema-aware and leaves the bare type standing. The same fix is applied to
`CREATE TYPE ... AS ENUM`, which had the identical last-part-only bug.

#### Fixed

- Schema-qualified `CREATE TYPE s.t AS RANGE` / `AS ENUM` no longer collides
  with a bare `t`; the two are distinct types, stored and duplicate-checked per
  `(schema, name)` with a schema-qualified catalog `_id`. `CreateRange` and
  `CreateEnum` thread the schema qualifier through the planner instead of
  dropping it, mirroring the composite-type template.
- `to_regtype` resolves a schema-qualified range or enum reference in bare,
  `schema.name`, and quoted `"schema"."name"` forms; a bare name resolves only
  the `public` type (matching the default search_path). `DROP TYPE schema.name`
  targets the schema-qualified type; `public.name` normalises to the bare name.
