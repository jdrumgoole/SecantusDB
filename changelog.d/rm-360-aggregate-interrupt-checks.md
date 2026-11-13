### Rust MongoDB server: an aggregate's line counts mongod's interrupt checks

An `aggregate` of a collection, and the `getMore`s of its cursor, now print mongod 8.2.11's `numInterruptChecks`. That covers a pipeline mongod's query layer runs whole (a `$match`, `$sort`, `$project` or `$limit` its planner takes, a `$group` its slot-based engine takes) and one read through its cursor stage whose later stages are `$project`, `$unset`, `$addFields`, `$set`, `$replaceRoot`, `$replaceWith`, `$unwind` and `$skip`. Through the cursor stage, each buffer the stage fills takes the global IS, as mongod's `DocumentSourceCursor::loadBatch` does, so a `getMore` of such a cursor takes no lock of its own. Its line then shows mongod's `locks`: none, and no storage-statistics read, when it filled no buffer. Each stage's `getNext` ticks the pipeline's interrupt checker, which checks every 128th tick and goes on with the cursor from batch to batch.

A walk of a whole index (the scan a sort is read in, or a hinted index with no predicate on its leading field, which mongod builds outside its access planner) no longer counts a planning check. Before, a `find`, `count` or `distinct` that read one printed one check more than mongod.

#### Fixed

- `secantus-core::deadline`: `tick_stages`, the pipeline checker's place kept and resumed (`ticks_to_check` / `resume_ticks`), the operation's `pipeline_counted` flag, and `take_expression_ticked`. With that flag, a pipeline whose `$map`, `$filter` or `$reduce` ticked the checker prints no count: mongod 8.2.11 makes no tick there.
- `secantus-commands`:
  - `agg_stream`: each cursor-stage refill takes a global IS (`lock_manager::global_read`, which fails the read when refused), and the stages run one at a time. Each document carries the `getNext` calls that pulled it out, counted when a batch returns it. A `$skip` or an `$unwind` of an empty array owes its calls to the next document it passes on, or to the pipeline's end.
  - `StreamCursor::locks_internally`: a `getMore` of such a cursor takes no statement lock. It yields the global IS it reads under (`lock_manager::read_yields`).
  - `aggregate`: a pipeline the query layer runs whole is counted (`explain::query_layer_takes_whole`). A collection that is not there makes no planning check, as a `find`'s does not.
  - `explain::IxScan::whole` marks a walk of a whole index, and `planning_checks` counts no leaf for it.
- `tests/backlog/opdebug.py::logged` takes `keep`, so RM-360's gap test can compare a `createIndexes` line, which it always leaves out otherwise. Its `createIndexes` shape no longer builds an index in its setup.
- Narrows RM-360 (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks, tests/mongod_live/test_interrupt_checks.py::test_a_line_counts_mongods_interrupt_checks, tests/backlog/test_rm_355_364.py::test_every_line_counts_its_interrupt_checks).

#### Filed

- RM-579: an aggregate through its cursor stage counts two execution admissions where mongod counts one more for each buffer, because this server holds the command's locks to its end (tests/backlog/test_rm_found_2026_11_12_rm360.py::test_each_cursor_stage_buffer_takes_an_execution_ticket).
- RM-580: explain leaves a `$match` after a `$redact` where it was written; mongod copies it ahead and into the query (tests/backlog/test_rm_found_2026_11_12_rm360.py::test_a_match_after_a_redact_is_copied_ahead_of_it).
