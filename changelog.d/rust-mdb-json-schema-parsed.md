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
- `features` with `oidReset` replaces the ObjectId process value and
  reports the old machine value beside the new one, as mongod 8.2.11 does;
  the ObjectIds the server mints come from one generator that carries it
  (`secantus_core::oid`; the storage crate's `_id` minting uses it too).
  `features` has no `js` block, as a `mongod --noscripting` answers. RM-143
  part (tests/backlog/test_rm_029_056.py::test_features_oid_reset,
  ::test_features_reports_the_script_engine).
- An `$or` that is the whole query is planned branch by branch with each
  branch's own trial of the plans that could answer it, as mongod's
  subplanner does, and a branch read through an index of its own comes in
  the order of that index; `explain` and the order documents come back in
  follow it. Fixes RM-001
  (tests/backlog/test_rm_001_028.py::test_or_branch_index_choice_matches_mongod).
  An `$or` beside another clause is filed as RM-199.
- `.github/workflows/test.yml`: the Rust concurrency-invariants step's
  comment named the wrong reason for `-k rust` (the wheel does ship the
  SWIG binding; the `[python]` params run in the main lanes). RM-021.
- `secantus-core-py`, the parity binding, decodes BSON literally
  (`secantus_core::bsondoc::from_bytes`) where it went through serde, which
  reads a sub-document that looks like extended JSON as the value it stands
  for; `tests/test_rust_literal_bson_decode.py` now covers every MongoDB-side
  crate, the bindings included. Fixes RM-058.
- CI: the `test` and `test-durable` lanes set up Go with setup-go's module
  cache and download the cross-driver Go smoke tests' modules before the
  tests run, so a module proxy error can no longer fail a test. RM-012.
