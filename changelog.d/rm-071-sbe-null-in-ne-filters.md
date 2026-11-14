### Rust MongoDB server: an aggregate behind `{a: null}`, an `$in` or a `$ne` explains and runs in SBE

mongod 8.2.11 pushes a `$group` or `$count` behind these filters into its
slot-based engine. The Rust MongoDB server now builds and runs the same
`filter` programs: `typeMatch` folding undefined and missing values to
null, `isMember` against the plan's `InList`, and `!` over the traversal a
`$ne` becomes. `explainVersion`, `queryPlan`, `slotBasedPlan` and the SBE
execution stats now match mongod's for these pipelines.

#### Fixed

- `explain` of an `aggregate` whose `$match` is `{a: null}`, `{a: {$ne: c}}`
  (null included) or `{a: {$in: [...]}}` in front of a pushed-down `$group`
  or `$count` is the slot-based plan mongod gives, not the classic cursor.

Fixes RM-071 in part (tests/mongod_live/test_sbe_filtered_front_explain.py::test_filtered_front_explain_matches_mongod); the rest of the item stays open under tests/backlog/test_rm_057_084.py::test_explain_of_a_filter_sbe_runs_beyond_comparisons.
