### `explain` shows the memory a `$group` and a window held

An aggregate's `explain` now reports what mongod 8.2 reports for its two
memory-hungry stages. A `$group` lists each accumulator's most memory across
all its groups (`maxAccumulatorMemoryUsageBytes`) and the size of the
documents it returned (`totalOutputDataSizeBytes`), the latter sized as mongod
sizes a document built field by field, to the byte.

A `$setWindowFields` is shown as mongod parses it: a `$sort` on its partition
and `sortBy`, which goes into the query's plan as a `SORT` when it can, even
behind an `$unwind`, then the `$_internalSetWindowFields` that runs the windows.
The window stage lists each function's most memory, the stage's total at its
peak, and its spill counters. A pipeline can now also name
`$_internalSetWindowFields` itself, as mongod allows.

#### Added

- `secantus-core`: `$_internalSetWindowFields`, a window stage that takes its
  input in the order it comes, with a partition for each run of one key.
- `secantus-core` `memlimit`: per-stage `GroupStats` / `WindowStats` in the
  spill report, and `built_document_size`, mongod's size of a document built
  field by field (measured on 8.2.1 over 1 to 14 fields and names of 1 to 100
  bytes).

#### Fixed

- `explain` (Rust MongoDB server): `$group`'s `maxAccumulatorMemoryUsageBytes`
  and `totalOutputDataSizeBytes`; `$setWindowFields` desugared into
  `$addFields` / `$sort` (`outputSortKeyMetadata`) / `$_internalSetWindowFields`
  / `$project` as mongod 8.2 parses it, with the sort absorbed into the query's
  plan, the outputs printed as mongod prints them back, and
  `maxFunctionMemoryUsageBytes`, `maxTotalMemoryUsageBytes` and the spill
  counters.
- Fixes RM-328 (tests/backlog/test_rm_found_2026_10_26.py::test_explain_memory_counters),
  with the values pinned in `tests/mongod_live/test_explain_memory_counters.py`.
- Filed while measuring: RM-408 (a document a stage changed or built is sized
  as if read), RM-409 (a classic `$first` / `$last` counts its value on mongod),
  RM-410 (a pipeline's own `$_internalSetWindowFields` ranks without a sort key).
