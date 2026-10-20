### The Rust MongoDB server parses `$jsonSchema` as mongod does in `explain`

`explain`'s `parsedQuery`, and the query tree mongod's planner prints when
it refuses a query, show a `$jsonSchema` as the predicates mongod parses it
into: `required` as `$exists`, a property's keywords as mongod's
`$_internalSchema*` expressions holding only where the field is of their
type (unless the schema declares that one type), a nested object as
`$_internalSchemaObjectMatch`, and the combinators as `$or`, `$and` and
`$_internalSchemaXor`. The schema had been echoed as written.

#### Fixed

- `secantus-commands` `explain/json_schema.rs`: the translation, transcribed
  from what mongod 8.2.11 prints for 71 schemas; the planner's tree text for
  each `$_internalSchema*` expression; mongod's tree order, which compares
  two children of one type and path by their own children. Fixes RM-196
  (tests/backlog/test_rm_planner_text.py::test_json_schema_is_parsed_as_mongod_parses_it).
- The planner's tree text for `$where` (`$where` / `code: ...`).
