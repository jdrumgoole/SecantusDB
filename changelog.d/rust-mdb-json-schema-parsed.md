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
- The embedded Python `RustServer` handle takes `noop_heartbeat_seconds`:
  the oplog's noop heartbeat, which also prunes the oplog, as the daemon's
  `--noop-heartbeat-seconds` writes it (off by default, as there). Its TTL
  monitor was already there. Fixes RM-114
  (tests/test_rust_server_smoke.py::test_noop_heartbeat_is_written_only_when_asked).
- RM-117 closed by measurement: the four write-command probes that found 21
  divergences on 2026-09-02 (`update_operators`, `arg_types_documents`,
  `update_path_conflicts`, `findandmodify_shapes`, 155 shapes) find none on
  the Rust server against mongod 8.2.11 (2026-10-20), so the two never named
  are gone.
- **TTL expiry bounds are mongod's** (fixes RM-104): a date exactly
  `expireAfterSeconds` old now expires (mongod's index scan includes both
  keys), the seconds are whole seconds, and a clustered collection expires
  only `_id` dates from the epoch on, so an `_id` dated 1960 stays, as on
  mongod 8.2.11 (`crates/secantus-storage/tests/ttl.rs::boundary_is_inclusive`,
  `::a_clustered_collection_expires_from_the_epoch`).
- **`upsertSupplied` is one statement** (fixes RM-102): the supplied document
  is inserted by the same storage statement as the match, where it used to
  be a second insert after it, so a concurrent insert between the two could
  turn it into a duplicate-key error. It also takes the `_id` a dotted
  equality builds (`{"_id.x": 1}` upserts `_id: {x: 1}`, as mongod 8.2.11
  does; it minted an ObjectId)
  (`crates/secantus-storage-adapter/tests/command_crud_wt.rs::update_constants_and_upsert_supplied`).
- RM-049 closed as a scope decision: mongod's order for documents that tie
  under a sort with a limit is its C++ library's heap, and a Linux mongod
  8.2.12 (libstdc++) and a macOS mongod 8.2.11 (libc++) disagree on 309 of
  540 measured cases, so no order is mongod's (Joe's decision of
  2026-10-16 for that outcome). Its two gap tests, which asserted the macOS
  answer, were retired with it.
- **An `$or` beside other clauses is a candidate plan** (fixes RM-199): the
  plan that reads each branch's index, with the clauses beside the `$or`
  pushed down into each branch and rechecked above the `OR`, is now tried
  against the plans of one index, as mongod 8.2.11 tries it. The
  `find_order` probe is at 62 differing replies of 6,531, all of them ties
  under a sort with a limit (RM-049's scope decision); it was at 77
  (`tests/backlog/test_rm_001_028.py::test_or_beside_other_clauses_is_a_candidate_plan`).
- A query that is nothing but `$alwaysTrue` keeps it in `parsedQuery`, as
  mongod does; an `$and` of it is still `{}`
  (`explain::parsed::tests::always_true_stays_only_alone`).
