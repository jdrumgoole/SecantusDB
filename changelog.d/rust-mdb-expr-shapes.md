### Rust MongoDB server: expressions and projections are parsed as mongod parses them

mongod parses every expression and projection in a command before it reads a
document. The Rust server read an object it could not place as a literal
value, so `{$addFields: {y: {$add: [1], $b: 2}}}` returned the object where
mongod refuses the stage, and on an empty collection nothing was checked at
all. Probing every position that parses an expression found the same gap in
`$project`'s own grammar. It also found a wrong answer: `{$project: {a: {b:
1}}}` returned `a: {b: 1}` whatever `a.b` held, because a sub-document was
computed as a literal rather than read as a sub-projection. `find` refused
every nested projection.

A new module, `secantus-core`'s `expr_parse`, follows mongod's parser
(`expression.cpp`, `add_fields_projection_executor.cpp` and
`projection_parser.cpp`, 8.2.11) in mongod's order. `aggregate`, `find`,
`findAndModify` and update pipelines run it before anything else reads the
command.

#### Fixed

- An object whose first field names an operator may have no other field
  (15983); any other object is a literal whose field names are field paths
  (16410, 16412, 15998) and may not repeat (16406). This applies in
  `$addFields`, `$set`, `$project`, `$replaceWith`, `$replaceRoot`,
  `$redact`, `$expr` (aggregate, `find`, `findAndModify`), `$lookup`'s `let`
  and sub-pipeline, `$unionWith`, `$facet`, `$setWindowFields`, the `$group`
  stages and update pipelines. An unknown operator and a missing required
  argument are reported in the same walk, so the first error mongod meets is
  the one reported.
- `$addFields` / `$set` check their own structure first, as mongod's
  validator does: conflicting paths (40176), an operator beside another field
  (40181), a dotted name in a sub-object (40183), and a top-level name that
  is no field path (40352, 40353, 15998).
- A projection (`$project`, `find`, `findAndModify`) is parsed as mongod's
  projection parser builds its tree: path collisions (31249 / 31250, and
  `{"a.b": 1, a: {c: 1}}` is one sub-tree, not a collision), empty
  sub-projections (51270), child field names, inclusion against exclusion
  (31253, 31254) and against an expression (31252) or a literal (31310),
  positional projection in an aggregation (31324), and `$elemMatch` outside
  `find` (168).
- A sub-document in a projection is a sub-projection, classified leaf by
  leaf, in `$project` and in `find`; any nonzero number includes.
- A `$group` `_id` whose FIRST field is not an operator is an object
  literal: `{a: 1, $b: 2}` is 17390, as on mongod.
- Fixes RM-281 (`tests/backlog/test_rm_expression_parse.py::test_two_operator_object_everywhere`,
  `tests/backlog/test_rm_group_shapes.py::test_expression_object_with_two_operators`).
- Fixes RM-282 (`tests/backlog/test_rm_expression_parse.py::test_add_fields_spec_validation`).
- Fixes RM-284 (`tests/backlog/test_rm_expression_parse.py::test_projection_parse`).
- Fixes RM-285 (`tests/backlog/test_rm_expression_parse.py::test_nested_sub_projection`).

#### Filed

- RM-286: `$meta` beside an exclusion in `$project` is "not supported".
- RM-287: `find`'s `$slice` with a path, and a positional projection over a
  filter that never names the array.
- RM-280 now carries its sizing and the decision to model SBE fully: SBE
  answers 248 of 3,360 expression shapes differently from the classic
  engine, 27 of them by value.
