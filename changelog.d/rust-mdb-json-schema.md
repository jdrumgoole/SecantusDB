### Rust MongoDB server: `$jsonSchema` is translated and refused as mongod does

The Rust MongoDB server now handles `$jsonSchema` the way mongod does. It
translates the schema once, up front, into mongod's own tree of match
expressions, and matches that tree. Until now it interpreted the schema
document directly, and a probe of 287 schemas against mongod 8.2.11 found 24
that matched different documents and 97 that answered a different error, or
none.

The differences were of two kinds. A malformed schema (an unknown keyword, an
empty `enum`, `required: "a"`, a `multipleOf` of NaN, `type` beside `bsonType`)
was accepted and matched documents, where mongod refuses the query with its
own code and message. Now it is refused, by `find`, `count`, `distinct`,
`aggregate`, `update`, `delete`, `findAndModify` and a collection validator
alike, even over an empty collection. A valid schema could still match the
wrong documents:
- the names in `required` and `properties` are dotted paths that reach
  into arrays;
- `enum` and `uniqueItems` compare documents whatever their field order;
- `multipleOf` is computed in Decimal128, so `0.3` is a multiple of `0.1`;
- `minimum` and `maximum` now accept every numeric type, Decimal128 and NaN
  bounds included, with `$gte` / `$lte`'s rules.

#### Fixed

- `secantus-core` (`jsonschema.rs`, new): a port of mongod 8.2.11's
  `JSONSchemaParser` and of the `$_internalSchema*` matchers, which replaces
  the interpreter in `query.rs`. It checks keywords in mongod's order and
  gives each node its own rule for arrays on its path. It also covers
  `encrypt` and `encryptMetadata`, including their field checks.
- `secantus-commands`: the up-front filter check that `count`, `distinct`,
  `aggregate`, `update`, `delete` and `findAndModify` share now refuses a
  malformed `$jsonSchema`, as `find` already did for the few keywords it
  checked.
- `tools/probes/query_shapes_schema.py`: 66 more cases (the `encrypt`
  fields, integer arguments, top-level-only keywords, paths through arrays,
  pattern-property names). The whole query probe, 979 cases, now differs from
  mongod only on the five `$where` cases, which are refused by design.
- Fixes RM-268 (`tests/backlog/test_rm_json_schema.py::test_json_schema_answers`).
- Fixes RM-269 (`tests/backlog/test_rm_json_schema.py::test_json_schema_validation`,
  `tests/backlog/test_rm_json_schema.py::test_json_schema_refused_by_every_command`).
