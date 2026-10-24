### Rust MongoDB server: find's positional and `$slice` projections, and `$meta` in an exclusion, as mongod has them

A positional projection (`{"arr.$": 1}`) now reads the element mongod reads:
the position its match of the query recorded (`MatchDetails::elemMatchKey`).
That is the first matching element of the first array on a leaf's path, and
the last leaf to record one wins, in the optimizer's order (match type, then
path). So `find({"o.k": 2}, {"arr.$": 1})` returns the `arr` element at `o`'s
matching position, and a filter that never names the projected array is no
longer refused. The Rust server used to re-match the filter's clauses on the
projected array, and it refused, with a generic code 2, every positional
projection whose filter did not name that array.

#### Fixed

- `secantus-core` (`elem_match_key.rs`, `projection.rs`): the positional
  element as mongod picks it.
  - With no recorded position over an array, 51246; with a position past the
    array's end, 51247 (both per document).
  - A value no array holds is returned as it is, and a missing one is left
    out.
  - More than one positional is 31276, an exclusion 31395, and `$expr` in the
    filter beside one 224.
  - The projected document keeps the stored field order.
- `secantus-core` (`expr_parse.rs`, `projection.rs`): a find projection's
  `$slice` is find's `[skip, limit]` only when it is a number or two numbers
  with a positive limit.
  - Anything else is the aggregation `$slice` expression, a computed field:
    `{y: {$slice: ["$arr", 1]}}` computes `y`, where the server returned the
    document unprojected.
  - A `$slice` that is neither is 28667, or 31252 in an exclusion, naming both
    reasons as mongod does.
- `secantus-core` (`expressions.rs`): `$slice` reads its first two arguments,
  null winning, before it checks their types, and reads a third after.
  `{$slice: [1, null]}` is null.
- `secantus-core` (`aggregate.rs`): `$meta` beside an exclusion in `$project`.
  `textScore` is 40218, and metadata the document lacks leaves the field out.
  It was refused as not supported.
- `secantus-commands` (`find.rs`, `findandmodify.rs`):
  - An error a projection raises on a document carries mongod's executor
    wrapper ("Executor error during find command: <ns> :: caused by ::", and
    "Plan executor error during findAndModify :: caused by ::").
  - A projection may read the command's `let` and `$$NOW`, and an undefined
    variable in one is 17276.
  - `findAndModify` folds a projection's constant sub-expressions while it
    parses the command, as `find` does, so a failing one is reported bare.
- Fixes RM-286 (`tests/backlog/test_rm_expression_parse.py::test_meta_in_exclusion_project`),
  RM-287 (`::test_find_only_projection_operators`,
  `::test_positional_reads_the_position_the_match_recorded`,
  `::test_slice_is_find_syntax_or_the_expression`),
  RM-296 (`tests/backlog/test_rm_sbe.py::test_find_projection_error_carries_the_executor_wrapper`)
  and RM-297 (`::test_find_projection_reads_the_command_let`).
