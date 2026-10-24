### Rust MongoDB server: which variables mongod treats as constants while it optimizes

mongod substitutes the variables it knows before it reads a document: the
command's `let`, `$$NOW`, and a `$let` variable bound to a constant. It
binds the rest per document: `$map`'s and `$filter`'s `as`, `$reduce`'s
`$$this` and `$$value`, and a `$let` variable read from a field. The Rust
server got both halves wrong. An error raised through `$$this` was reported
as the optimizer's ("Failed to optimize pipeline"), and `find` raised it on
an empty collection. A constant reached through the command's `let` was never
folded, so `{$divide: [1, "$$z"]}` with `let: {z: 0}` answered `[]` on an
empty collection, where mongod refuses the command.

#### Fixed

- `secantus-core` (`expressions.rs`): the evaluator tracks which variables
  in scope are bound per document. An error through one carries the
  executor's wrapper, and `find` no longer raises it over an empty
  collection.
- `secantus-core` (`expr_rewrite.rs`), `secantus-commands` (`aggregate.rs`,
  `find.rs`, `findandmodify.rs`, `argtypes.rs`): the optimizer's check of
  constant sub-expressions substitutes the command's `let` (and `$$NOW`, and
  a constant `$let` binding) first. This applies in `aggregate` (`$match`,
  `$project`, `$group`, `$unionWith`, `$facet`, ...), in `find`'s filter and
  projection, and in `findAndModify`'s query. `update` and `delete` still
  answer `n: 0`, as mongod does.
- Fixes RM-293 (`tests/backlog/test_rm_sbe.py::test_a_variable_bound_from_input_is_not_constant`,
  `::test_a_command_let_variable_is_folded_while_optimizing`).

#### Filed

- RM-296: a `find` projection's per-document error lacks mongod's "Executor
  error during find command" wrapper.
- RM-297: a `find` projection cannot read the command's `let`.
