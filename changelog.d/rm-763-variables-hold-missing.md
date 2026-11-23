### The variables a stage hands its sub-pipeline can be MISSING

mongod keeps each variable as a `Value`, which can be EOO (MISSING) as well as any
BSON value. This server kept them as a document of values, which has no MISSING, so
a variable bound from an absent field was null in a `$lookup` sub-pipeline.
They are now a `Vars` (`secantus_core::vars`): a `Field` (`Missing` or `Value`) per
name, threaded through the stages, `$expr` filters, projections, updates and
storage reads in place of the old `vars: &Document`. The expression engine's own
`Ctx::missing` list is gone with it.

#### Fixed

- A `$lookup` `let` variable bound from an absent field reads `"missing"` in
  `$type` and is omitted by `$project`, as on mongod 8.2.11; it was null.
- A command `let` of `$$REMOVE` is MISSING in `aggregate`, `find`, `update`,
  `findAndModify` and the stages inside them.
- A `$unionWith` sub-pipeline sees the stage's variables (the command's `let`
  and an enclosing `$lookup`'s), as mongod's does; it saw `$$NOW` and
  `$$CLUSTER_TIME` only, and a streamed `$unionWith` none, which answered "not
  supported by the Rust server".
- A `$graphLookup` `restrictSearchWithMatch` with an `$expr` reads the stage's
  variables; it ran with none.
- A `$merge` `let` variable bound to MISSING is not among the `whenMatched`
  pipeline's variables ("Use of undefined variable"), as on mongod.

Fixes RM-763 (tests/backlog/test_rm_found_2026_11_22_rm635.py::test_a_lookup_let_variable_bound_from_an_absent_field_is_missing)
