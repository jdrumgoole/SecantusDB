### explain: a `$cursor` the pipeline stops reading reports the batch it loaded

A `$cursor` whose following stages stop pulling (a `$limit` after an
`$unwind`) now reports the plan as mongod 8.2.11 does: run only through the
batches the cursor loaded, with each stage's work accounting.

#### Fixed

- Under `[{$match: {a: {$gt: 2}}}, {$unwind: "$b"}, {$limit: 3}]` the
  `$cursor`'s FETCH and IXSCAN report `nReturned` 32, 32 works, `saveState`
  2, `restoreState` 1 and `isEOF` 0, not a run to the plan's end with no
  work counts.

Fixes RM-692 (tests/backlog/test_rm_found_2026_11_14_rm434.py::test_a_cursor_the_pipeline_stops_reading_reports_one_batch)
