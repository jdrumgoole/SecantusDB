### Rust MongoDB server: a refused write explains itself exactly as mongod does

When a write fails a collection's validator, mongod's `errInfo` says which
rules the document broke. The Rust MongoDB server's explanation now matches
mongod 8.2.11 exactly, key order included, on all 183 failing documents in
`tools/probes/validation_error_details.py`. Before this change, 34 of the
first 173 differed.

The `$jsonSchema` explanation had been judging keywords by rules of its own,
not the matcher's. It reported a `multipleOf` failure for `0.3` against
`0.1`, missed duplicates that differ only in field order, read
`properties: {"a.b": ...}` as a field named `a.b`, and still tested
`pattern` under `bsonType: "int"`, which mongod drops. It now asks the
matcher's own rules. A sub-schema's `title` and `description` now appear
where mongod puts them, and `additionalProperties` is reported before
`patternProperties`.

#### Fixed

- `secantus-commands` (`validation_errors.rs`): the `$jsonSchema`
  explanation uses `secantus-core`'s `multipleOf` (Decimal128) and its
  `enum` / `uniqueItems` equality. Dotted names in `properties`, `required`
  and `dependencies` are paths. A keyword that the schema's single stated type
  rules out is dropped. A top-level `enum` with no documents in it is
  reported as "expression always evaluates to false". A value that is not
  encrypted fails `encrypt` with "value was not encrypted".
- `secantus-commands` (`validation_errors.rs`): query-operator explanations.
  - `$size`, `$elemMatch`, the bit operators, `$mod` and `$regex` over a value
    of a type they cannot match report "type did not match" with the expected
    types.
  - `$geoWithin` / `$geoIntersects` have mongod's reasons.
  - A numeric path component reads each element's field before the position,
    and a branch that lacks the path ends the considered values, as mongod's
    path iterator does.
  - A `$jsonSchema` that matched under `$nor` is "schema matched".
- `secantus-commands` (`crud.rs`): a validator holding `$alwaysTrue` or
  `$alwaysFalse` cannot be explained. The refusal now carries mongod's
  `note` and its 4994600 error, not an invented explanation.
- `tools/probes/validation_error_details.py`: 112 more failing documents.
- Fixes RM-270 (`tests/backlog/test_rm_validation_errors.py::test_schema_rules_judged_as_the_matcher_judges`).
- Fixes RM-271 (`tests/backlog/test_rm_validation_errors.py::test_schema_explanation_shape`).
- Fixes RM-272 (`tests/backlog/test_rm_validation_errors.py::test_query_operator_explanations`).
- Fixes RM-273 (`tests/backlog/test_rm_validation_errors.py::test_unexplainable_validator_note`).
