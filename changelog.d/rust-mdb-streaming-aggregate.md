### Rust MongoDB server: an `aggregate` cursor reads on as `getMore` asks

An `aggregate` cursor used to be the whole result, read when the command
ran, so a `getMore` never saw a write made since. Now a pipeline of
streaming stages reads the collection as mongod does (measured 8.2.11): batch
by batch when the query planner takes the pipeline whole or SBE takes the
rest, and otherwise through mongod's cursor-stage buffer, which reads 32
documents, then twice as many at each refill, ending a refill at the document
that brings it to 4 MB. The stages after the read run on each document only
as the cursor reaches it, so an error in a later document is the `getMore`'s,
as on mongod.

#### Changed

- `aggregate` streams a pipeline of `$match`, `$project`, `$unset`,
  `$addFields` / `$set`, `$replaceRoot` / `$replaceWith`, `$unwind`,
  `$redact`, `$skip`, `$limit`, `$lookup` and a `$sort` its plan reads in
  order. Fixes part of RM-080
  (`tests/backlog/test_rm_057_084.py::test_aggregate_get_more_sees_writes_made_since_the_command`,
  `::test_aggregate_pipelines_read_on_as_mongod_reads_them`,
  `::test_aggregate_cursor_stage_buffers_as_mongod_does`,
  `::test_aggregate_stages_run_as_the_cursor_is_read`).
- `explain` moves a `$match` ahead of a `$project` or `$unset` that keeps
  every field it reads (onto the field a rename names) and ahead of a
  `$sort`, so the query reads it, as mongod does; the read uses an index on
  it. Fixes RM-307
  (`tests/backlog/test_rm_found_2026_10_24.py::test_explain_moves_a_match_ahead_of_a_projection_that_keeps_its_fields`).

#### Fixed

- An error a streamed `find` or `aggregate` cursor meets in a later batch
  is worded as the `getMore`'s (`Executor error during getMore :: caused by
  :: ...`), not as the opening command's. Fixes RM-306
  (`tests/backlog/test_rm_found_2026_10_24.py::test_a_streamed_get_more_names_itself_in_an_executor_error`).
