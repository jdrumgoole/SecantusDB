### An unknown expression operator gets mongod's code, name and envelope

mongod does not answer this one way, and the discriminator is **position**: the
top-level value of a `$project` field is parsed by the projection parser, which
has its own code and wording, while anywhere deeper the generic expression
parser answers. The same `$project` therefore gives two different codes
depending on how deep the unknown operator sits.

#### Fixed

- `{$project: {n: {$nosuch: 1}}}` now answers `31325 Invalid $project :: caused
  by :: Unknown expression $nosuch` — mongod's code and its wording, with the
  operator unquoted — instead of `168`. Nested deeper, and in `$addFields` /
  `$set`, it still answers `168 ... Unrecognized expression '$nosuch'`, which is
  also what mongod does. `$count` / `$topN` / `$bottomN` follow the same rule.
- `codeName` for code 168 is now `InvalidPipelineOperator` rather than the
  generic `Location168`. This was wrong for every unknown-expression error.
- The message envelope is now mongod's: `Invalid $addFields :: caused by ::`
  where the stage supplies one, and no envelope at all in `$group` /
  `$replaceWith` / `$expr`. An unknown operator was previously found by the
  constant folder, which stamped `Failed to optimize pipeline :: caused by ::`
  on all of them.

Swept by `tools/probes/unknown_expression_errors.py` (13 shapes, Python 0
divergent, down from 13); gated by `tests/test_mongod_differential.py -k
unknownexpr`.
