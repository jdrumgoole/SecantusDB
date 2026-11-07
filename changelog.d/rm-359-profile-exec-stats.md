### A profile entry says what each stage of its plan did

The Rust MongoDB server's profile entries now carry `execStats`, as mongod
8.2's do: the plan's stages, each with how many times it worked, how many
results it handed up, how many works needed more time, how often it yielded,
whether it reached its end and what it examined. The numbers are counted as
the plan runs, by the same stages that read the documents, not worked out
afterwards, so a `find` and each `getMore` of its cursor, a `count`, a
`distinct`, every update and delete statement and a `findAndModify` report
the run they actually had. Compared field by field with mongod 8.2.11 over
more than fifty shapes of read and write, they agree.

#### Added

- `execStats` in the `system.profile` entry of a streamed `find` and each of
  its `getMore`s (cumulative, as a cursor's plan runs on), a `count`
  (`COUNT` over its read, or `RECORD_STORE_FAST_COUNT`), a `distinct`, each
  `update` and `delete` statement (`UPDATE`, `DELETE`, `BATCHED_DELETE` and
  the `_id` express writes, with `nMatched`, `nWouldModify`, `nWouldUpsert`,
  `nWouldDelete`) and a `findAndModify`. mongod's rules are kept: a FETCH
  works once more than a SORT_MERGE, which knows it is at its end without a
  work; `find`'s executor works its LIMIT once past the last result and a
  `getMore`'s does not; a write's input yields around each document it
  writes, a BATCHED_DELETE's once per ten; a projection's `transformBy` is
  `{}`. Each stage's `executionTimeMillisEstimate` is timed on a coarse
  clock, as mongod's fast clock source times it. An `aggregate` and an
  insert carry none, as on mongod.

#### Fixed

- Fixes RM-359 (tests/backlog/test_rm_355_364.py::test_a_profile_entry_carries_the_execution_stats,
  tests/backlog/test_rm_355_364.py::test_a_profile_entry_reports_each_stage_as_it_ran,
  tests/backlog/test_rm_355_364.py::test_a_sort_of_index_keys_reports_its_works,
  tests/backlog/test_rm_355_364.py::test_an_aggregate_or_an_insert_has_no_execution_stats).

#### Changed

- The planner's index catalog is shared through `Arc` rather than `Rc`, so a
  cursor can keep the plan it runs.

Filed while working: RM-377 (a DISTINCT_SCAN's key past its bounds), RM-373
(where a projection sits in the plan), RM-374 (a SORT of one document counts
no data), RM-375 (a SORT of index keys has no `totalDataSizeSorted`), RM-376
(an update with a `sort`, and an `_id` update under a collation, planned as
an express write).
