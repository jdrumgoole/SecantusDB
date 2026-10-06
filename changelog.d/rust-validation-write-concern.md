### Validation failures, validators and write concern now answer as mongod does, on the Rust server

A write that leaves a document failing the collection validator now gets
mongod's full reply on every update path: `update` (operator, replacement,
pipeline, multi, upsert) and `findAndModify`. That means the `Plan executor
error during <command>` prefix and the `errInfo` explaining which schema rule
failed. Before this, updates carried no `errInfo` at all.

`create` and `collMod` now parse the validator. An invalid `$jsonSchema` or
an unknown operator is refused when it is set, as on mongod, instead of being
stored and then failing every write. `writeConcern.w` follows mongod's
parsing. Decimal `$log` is answered rather than refused.

#### Fixed

- Rust server: validation failures on `update` and `findAndModify` carry
  mongod's message prefix and `errInfo` (`failingDocumentId` and the
  `schemaRulesNotSatisfied` tree).
- Rust server: `create` / `collMod` refuse a validator mongod cannot parse.
  This covers `$jsonSchema` `type: "integer"`, an unknown keyword and an
  unknown operator; `collMod` uses mongod's `Parsing of collection validator
  failed` prefix.
- Rust server: `writeConcern.w` accepts a double or decimal (truncated), null
  (the empty tag) and a tag set, and refuses bool / array / an empty tag set
  with mongod's 9.
- Rust server: `$log` with a decimal operand returns a decimal result.
- Rust server: documents sort as mongod sorts them, comparing each element's
  value type before its field name. GeoJSON with a `crs` member used to sort
  after a point without one.
