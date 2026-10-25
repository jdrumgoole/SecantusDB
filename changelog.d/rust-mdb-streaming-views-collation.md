### Rust MongoDB server: cursors through a view and under a collation read on as mongod's do

A `find` or an `aggregate` through a view, or with a collation (the
command's, a view's or a collection's default), now reads on as `getMore`
asks, as one over a collection already did: batch by batch where mongod's
planner takes the read whole, through mongod's cursor-stage buffer
otherwise. A `getMore` sees documents written since the command, and a
large result is no longer held whole.

#### Fixed

- A cursor through a view or under a collation streams (RM-080,
  `tests/backlog/test_rm_found_2026_10_25.py::test_a_cursor_through_a_view_or_under_a_collation_reads_on`).
  A collated index streams when its bounds hold only numbers or dates.
- A `$group` mongod reads from a DISTINCT_SCAN, through a view, returns its
  groups in the scan's order (RM-309,
  `tests/backlog/test_rm_found_2026_10_25.py::test_a_distinct_scan_group_through_a_view_keeps_the_scans_order`).
- Dropping the collection beneath a view, or an index a view's cursor
  reads, ends that cursor with mongod's 175; it used to end it quietly, its
  `getMore` returning no documents. Dropping the view no longer ends it. A
  cursor read through the cursor stage, directly or through a view, returns
  what it had buffered when its collection or index is dropped, and fails
  at the first `getMore` that needs more, as on mongod (RM-080,
  `tests/backlog/test_rm_found_2026_10_25.py::test_a_cursor_returns_what_it_buffered_when_its_collection_is_dropped`).

#### Known differences

- RM-080: a collated index bounded on strings still reads every document
  at the command.
- RM-312 (found here, needs a decision): a blocking sort keeps equal
  documents in arrival order; mongod's unstable sort does not.
