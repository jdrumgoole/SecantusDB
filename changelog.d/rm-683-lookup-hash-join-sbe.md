### A `$lookup` on join fields is explained as a hash join in SBE

`explain` of a `$lookup` on `localField` / `foreignField` over a collection
scan now reports what mongod 8.2.11 does when it joins by hash:
`explainVersion` "2", no classic `$cursor` and `$lookup`, an `EQ_LOOKUP`
(strategy `HashJoin`, or `NonExistentForeignCollection`) over the query's
plan, and the slot-based plan it runs: `hash_lookup`, `nlj`, `group`,
`branch`, `union`, `unwind`, `limit` and `coscan` with their slots and lambda
frames, one set for each `$lookup` in a row. `executionStats` counts what each
stage did in a run of the foreign side's key program over both collections:
a foreign record whose field is an array, a missing or empty one, or a
nested path takes the stages mongod's does, and each stage reports its opens,
closes and returned rows.

#### Added

- The `hash_lookup`, `nlj`, `branch`, `union`, `unwind` and `coscan` SBE
  stages, and the `let` expression, with the printer's layout for them, in
  `explain/sbe`.

#### Fixed

- A hash-join `$lookup` was explained as a classic `$cursor` and `$lookup`
  stage. Closes the hash-join part of RM-683 (tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_lookup_on_join_fields_is_explained_as_sbe_runs_it,
  tests/mongod_live/test_lookup_hash_join.py); the other join strategies
  stay open under RM-683, and what the work found is filed as RM-827,
  RM-828 and RM-829.
