### A pushed-down `$group` is explained as the slot-based plan mongod runs

mongod 8.2 runs a `$group` behind a collection scan in its slot-based
execution engine (SBE), and `explain` says so: `explainVersion` "2", a
winning plan made of the query solution and the slot program itself, and
execution statistics counted by the SBE stages that ran. The Rust MongoDB
server used to answer such an explain in the classic shape, a `$cursor`
followed by a `$group` stage, so a tool reading the plan saw an engine
mongod does not use.

It now builds that slot program as mongod 8.2.11's stage builders build it
(slot by slot, frame by frame), prints it as mongod's debug printer does,
and runs it over the collection, so `nReturned`, `numReads` and the
`$cursor` batches behind a classic stage all come from executing the plan.
This covers a `$group` over a whole-collection scan whose key and
arguments are field paths, `$$ROOT` or constants, with the common
accumulators. Filtered fronts, `$lookup`, `$count`, `$sortByCount` and
`$bucket` are still explained in the classic shape.

#### Fixed

- `secantus-commands` (`explain/sbe/`): `explain` of an `aggregate` whose
  `$group` mongod pushes into SBE (a whole-collection scan, `_id` a field
  path, `$$ROOT`, a scalar constant or an object of those, `$sum`, `$count`,
  `$avg`, `$first`, `$last`, `$min`, `$max`, `$push`, `$addToSet`, at most
  one distinct dotted path) returns `explainVersion` "2", `queryPlan`,
  `slotBasedPlan` and SBE `executionStats` (`scan`, `project`, `group`,
  with `saveState` / `restoreState` counting the `$cursor`'s doubling
  batches), byte for byte with mongod 8.2.11. Any pushed-down pipeline over
  a collection that does not exist is the `limit 0` / `coscan` plan.
  Fixes RM-071 in part (tests/mongod_live/test_sbe_group_explain.py::test_group_explain_matches_mongod);
  the rest stays open (tests/backlog/test_rm_057_084.py::test_explain_of_a_group_pushed_into_the_query).
- Filed RM-435: a `$sort` after a pushed-down `$group` sizes its documents
  as the classic engine builds them
  (tests/backlog/test_rm_found_2026_11_08_rm071.py::test_a_sort_after_a_pushed_down_group_sizes_what_sbe_hands_it).
