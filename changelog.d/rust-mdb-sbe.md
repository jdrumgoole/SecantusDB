### Rust MongoDB server: pipelines mongod runs in its slot-based engine answer as that engine does

mongod 8.2 has two execution engines. When it can push a `$group` or
`$lookup` down, it runs the front of the pipeline in its slot-based engine
(SBE), and SBE answers its own way: `$mod` by zero is 4848403 there and 16610
in the classic engine, a `$substrBytes` bound that is not a whole number gives
`null` there and a rounded substring in classic, and a constant `n` of `0` in
`$firstN` fails before any document is read, even on an empty collection. The
Rust server answered the classic engine's way everywhere. It now decides, as
mongod does, which stages run in SBE, and answers those as SBE does.

On mongod 8.2.11, 248 of the 3,360 shapes of `tools/probes/expression_shapes.py`
answer differently in SBE. Run in SBE position (`--sbe`), the Rust server now
differs from mongod only in the last digit of five Decimal128 results, which it
answers the same way in the classic engine. A new corpus of 3,614 shapes for
the operators SBE compiles, `tools/probes/expression_shapes_sbe.py`, leaves
only SBE's hash-ordered set expressions, the same last-digit results, and one
message mongod sends as bytes that are not UTF-8.

#### Fixed

- `secantus-core` (`sbe.rs`): mongod's pushdown rule (`sbe_pushdown.cpp`,
  `trySbeRestricted`). The find layer takes the leading `$match`es, a
  `$sort`, `$skip` / `$limit` and one inclusion or computed `$project`. After
  it, each `$group` / `$lookup` whose every operator SBE compiles is pushed
  down; the expression operators SBE does not compile keep a stage classic.
  `aggregate` runs those stages, and the read of the collection, in SBE; a
  `$unionWith` sub-pipeline has a pushdown of its own.
- `secantus-core` (`sbe.rs`): the checks SBE makes while it builds the plan,
  before any document is read, so they fail on an empty collection too, with
  no executor wrapper: a constant `n` (7548606), a constant null timezone of
  a date-part operator (5157900), and a constant `$dateToString` /
  `$dateFromString` format.
- `secantus-core` (`expressions/sbe_ops.rs`): every expression operator SBE
  compiles follows SBE's checks, in SBE's order, with SBE's codes and
  messages, and SBE's values. That covers arithmetic and `$round` / `$trunc`,
  the trigonometric operators, `$substr*`, `$indexOf*`, case conversion and
  `$strcasecmp`, `$concat*`, the regex operators, `$arrayToObject`, the set
  operators, `$in`, `$range`, `$replaceOne`, the trims, `$sortArray`,
  `$switch` and the date operators. A missing operand stays missing where
  SBE tells it from null. Constants the classic optimizer folds or checks
  first keep the classic answer.
- `secantus-core` (`group.rs`): in SBE, `n` is 7548606 / 7548607 and
  `$mergeObjects` of a non-object 5911200. Of equal values `$min` / `$max`
  keep the later one, `$stdDevPop` / `$stdDevSamp` step in doubles with no
  fused multiply-add, and `$topN`, `$bottomN`, `$maxN`, `$minN` keep and
  order ties as SBE's heap and `std::sort` do (libstdc++, the Linux build's;
  `stl.rs`).
- `secantus-core` (`group.rs`): in the classic engine, `$min` / `$max` keep
  the first of equal values and `$maxN` lists equal values latest first.
  `$bottom` / `$bottomN` keep the first of documents that sort equal in both
  engines.
- `secantus-core` (`expressions.rs`): `$replaceOne` / `$replaceAll` with a
  non-string argument fail with mongod's 10503904 / 10503901 / 10503902
  (they answered "not supported"); a field operand of `$setEquals` that is
  not an array is 17044, and only a constant one is the optimizer's 5887502,
  as a constant second operand of `$setIsSubset` is 17311; `$range` and the
  set operators name a missing operand "missing".
- Fixes RM-280 (`tests/backlog/test_rm_group_shapes.py::test_group_in_sbe_error_codes`,
  `tests/backlog/test_rm_sbe.py::test_pushdown_decides_the_engine`,
  `::test_operators_answer_as_sbe`, `::test_operators_stay_classic_when_not_pushed`,
  `::test_group_accumulator_codes`, `::test_accumulators_answer_as_sbe`,
  `::test_built_before_any_document`, `::test_n_accumulator_ties_follow_the_heap`).
- Fixes RM-288 (`tests/backlog/test_rm_sbe.py::test_bottom_keeps_the_first_of_equal_documents`).
- Fixes RM-289 (`tests/backlog/test_rm_sbe.py::test_classic_ties_keep_the_first`).
- Fixes RM-291 (`tests/backlog/test_rm_sbe.py::test_classic_operator_checks_found_beside_sbe`).

#### Filed

- RM-290: a constant timezone a date operator cannot use is refused by
  mongod's optimizer before any document is read; this server finds it only
  on a document.

#### Probes

- `tools/probes/expression_shapes.py --sbe` runs each shape in a `$group`
  mongod pushes into SBE, and records a reply the driver cannot decode
  instead of stopping.
- `tools/probes/expression_shapes_sbe.py`: 3,614 shapes for the operators SBE
  compiles.
