### Rust MongoDB server: `$group`, `$bucket` and `$bucketAuto` parse and answer as mongod does

A new probe, `tools/probes/group_shapes.py`, runs 282 `$group`, `$bucket`,
`$bucketAuto` and `$sortByCount` pipelines against mongod 8.2.11 and compares
the answers as canonical extended JSON. It found five causes of difference, all
fixed here. Two were wrong answers. Documents with the same fields in another
order were put in one group, where mongod keeps `{a: 1, b: 2}` and
`{b: 2, a: 1}` apart. `$bucketAuto` sized its buckets differently: ten values
in three buckets came out 4/3/3 where mongod makes 3/3/4.

The rest were refusals. A malformed accumulator was answered with the generic
"not supported", or was quietly run. That covered an unknown operator, two in
one field, an array argument, `$count` with arguments, and a bad
`$topN` / `$firstN` argument. mongod's
`$concatArrays` and `$setUnion` accumulators were missing. Each now behaves as
mongod's classic engine does, and the parse errors are reported before any
document is read, so an empty collection reports them too.

#### Fixed

- `secantus-core` (`group.rs`): a group key keeps a document's field order.
- `secantus-core` (`group.rs`): output fields are checked in mongod's
  accumulator-statement order (40234, 40235, 40236, 40238, 40237, 15952),
  then each accumulator's own argument. That covers `$count` (14), the
  N-accumulators (5787801, 5787900-5787907, 5788001-5788005, 10065,
  15974, 15975), `$accumulator` (4544703), and an expression object with
  two operator fields (15983) in `_id`, an argument, `$sortByCount` and
  `groupBy`. This applies to `$group`, `$bucket` and `$bucketAuto` alike.
- `secantus-core` (`group.rs`): an N-accumulator's `n` is evaluated against an
  empty document, as mongod's initializer is, and checked as the classic engine
  checks it (5787902, 5787903, 5787908, and 31109 for a constant NaN).
- `secantus-core` (`group.rs`): the `$concatArrays` and `$setUnion`
  accumulators. `$mergeObjects` of a non-object is 40400, and `$accumulator`
  is 31264 once a document reaches it, as on a `--noscripting` mongod. That
  follows the standing no-server-side-JavaScript decision.
- `secantus-core` (`group.rs`): `$bucketAuto` checks its arguments in the
  order written (40239-40246, 40257, 40261). Its bucket size is
  `round(documents / buckets)`, and the last bucket takes the rest.
- Fixes RM-275 (`tests/backlog/test_rm_group_shapes.py::test_group_spec_parse_errors`).
- Fixes RM-276 (`tests/backlog/test_rm_group_shapes.py::test_n_accumulator_arguments`).
- Fixes RM-277 (`tests/backlog/test_rm_group_shapes.py::test_group_key_document_field_order`).
- Fixes RM-278 (`tests/backlog/test_rm_group_shapes.py::test_bucket_auto`).
- Fixes RM-279 (`tests/backlog/test_rm_group_shapes.py::test_accumulators_new_and_refusing`).

#### Filed

- RM-280: a `$group` mongod runs in SBE fails with SBE's error codes.
- RM-281: an expression object with two operator fields, outside the group
  stages.
