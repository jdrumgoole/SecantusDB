### Aggregation stage-argument validation for `$count`, `$project`, and `$sortByCount`

Three more aggregation stages now reject malformed arguments with mongod's exact
error code instead of silently computing a wrong result. `$count` enforces that
its field is a non-empty string that is neither `$`-prefixed, dotted, nor the
reserved `_id`; an empty `$project` specification is rejected up front; and
`$sortByCount` requires a `$`-prefixed path string or a single-`$`-key expression
object. Both the Python and Rust servers are covered (the Rust core defers each
invalid case so the exact code is raised), and each is verified against real
mongod 7.0.12.

#### Fixed

- `$count` now raises `Location40156` (non-string field), `Location40157`
  (empty), `Location40158` (`$`-prefixed), `Location40160` (contains `.`), and
  `Location15948` (`_id`) instead of accepting the malformed field name.
- `$project` with an empty specification now raises `Location51272`
  ("projection specification must have at least one field") instead of returning
  the input documents unchanged.
- `$sortByCount` now raises `Location40149` (non-string/non-object argument),
  `Location40148` (bare, non-`$` path string), and `Location40147`
  (non-expression object) instead of grouping on a constant.
