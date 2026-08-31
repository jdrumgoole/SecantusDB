### Invalid `collation` specs are rejected instead of silently ignored

Every malformed collation spec was accepted: a missing `locale`, `strength: 9`,
`strength: "x"`, a misspelled field name. The query then ran under a *different*
collation than the caller asked for and reported success — so a typo in a
collation silently changed which documents matched.

#### Fixed

- `find`, `aggregate`, `count`, `distinct` and `findAndModify` now validate the
  collation spec's contents, matching mongod 8.2.11 exactly across 28 spec
  shapes: a required `locale` (an explicit `null` counts as missing, 40414),
  its type, `strength`'s type and range, strict-bool `caseLevel` /
  `normalization` / `numericOrdering` / `backwards`, the `caseFirst` /
  `alternate` / `maxVariable` enumerations, and unknown fields (40415).

  The rules are not symmetric and each was probed rather than inferred: an
  empty `{}` is accepted while `{strength: 2}` is not; `strength: 0` is an
  *enumeration* error where `6` is a *range* error; `strength: 2.5` is accepted
  but `strength: true` is not; and `backwards` uses a different wrong-type
  message from the boolean fields beside it.

- `update` and `delete` deliberately still accept **any** spec contents,
  because mongod does — validating them for consistency would reject specs a
  real server runs.

#### Known gaps

Collation *matching* is correct at every strength; collation **ordering** is
not ICU — accents sort after `z` rather than beside their base letter, and
`caseFirst`, `numericOrdering`, `backwards` and locale-specific rules are
ignored. An invalid `locale` name is also still accepted. Both are measured and
recorded in `tasks/backlog.md` §5, with the reason each was left rather than
approximated.
