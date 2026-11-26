### Explain moves and drops a `$match` as mongod's optimizer does behind a `$group` and a `$$ROOT.x` new root

The Rust MongoDB server's `explain` now leaves a later `$match` where mongod
8.2.11's optimizer leaves it in three more cases.

#### Fixed

- A `$match` of nothing (`{}`, or `{$sampleRate: 1}`) behind a `$group` or
  a `$set` is dropped, so the query takes the whole pipeline.
- A written `{$alwaysTrue: 1}` or `{$alwaysFalse: 1}` stays behind a
  `$group` instead of moving into the query.
- A `$match` behind `{$replaceRoot: {newRoot: "$$ROOT.x"}}` (or
  `$$CURRENT.x`) moves into the query renamed onto `x`, as behind `"$x"`.

Fixes RM-662 (tests/backlog/test_rm_found_2026_11_14_rm538.py::test_a_match_after_a_group_as_the_optimizer_leaves_it, tests/backlog/test_rm_found_2026_11_14_rm538.py::test_a_match_moves_ahead_of_a_root_variable_new_root)
