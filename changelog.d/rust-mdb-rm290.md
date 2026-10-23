### Rust MongoDB server: a date operator's constant arguments are checked before any document, in mongod's order

mongod's optimizer reads the constant arguments of a date operator that
reads the document before it reads anything: `unit`, then `startOfWeek`,
then `timezone`, then `binSize`. A bad one fails under "Failed to optimize
pipeline" even on a collection that does not exist, and a null one makes the
whole expression null, so nothing after it is checked. The Rust server
checked them only when it evaluated the operator on a document, so an empty
collection answered `[]`, and it checked `binSize` first and the timezone
before `unit`: `{$dateTrunc: {date: "$t", unit: null, timezone: "Mars"}}`
was an unknown-timezone error where mongod answers `null`.

#### Fixed

- `secantus-core` (`expressions.rs`, `expr_rewrite.rs`): the constant
  `timezone` of every date operator, and the constant `unit`, `startOfWeek`
  and `binSize` of `$dateAdd`, `$dateSubtract`, `$dateTrunc` and `$dateDiff`,
  are checked before any document is read, in mongod's order, wherever an
  expression sits in the pipeline. A variable a `$let` binds to a constant
  counts as one; a variable read from the document, or bound by `$map`,
  `$filter` or `$reduce`, hides it.
- `secantus-core` (`expressions.rs`): `$dateSubtract`'s `amount` of -2^63 is
  found per document, after a null or non-date `startDate` and an unknown
  `unit`, as on mongod; it was an optimizer error.
- Fixes RM-290 (`tests/backlog/test_rm_sbe.py::test_constant_timezone_refused_before_any_document`,
  `::test_date_arguments_checked_in_the_optimizers_order`).

#### Filed

- RM-293: an error raised through a variable bound from the input (`$map`'s
  `$$this`, a `$let` variable read from a field) is reported under the
  optimizer's wrapper instead of the executor's, and `find` raises it on an
  empty collection.
