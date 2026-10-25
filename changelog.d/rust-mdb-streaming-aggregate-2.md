### Rust MongoDB server: `$unionWith`, a DISTINCT_SCAN `$group` and an empty first batch read on as mongod reads them

An `aggregate` cursor reads more of its pipeline the way mongod 8.2.11 does.
A `$unionWith` opens its own collection only when the cursor reaches it, and
reads it through a buffer of its own. A `$group` that mongod answers from a
DISTINCT_SCAN returns its groups in the scan's order, read as the cursor
asks. A pipeline asked for a first batch of none runs nothing until the
first `getMore`.

#### Fixed

- A `$unionWith`'s collection is read when the cursor reaches it, through a
  cursor-stage buffer of its own (32 documents, then doubling). A document
  written to it after the command is returned, as on mongod (RM-080,
  `tests/backlog/test_rm_found_2026_10_25.py::test_a_union_with_reads_its_collection_when_the_cursor_reaches_it`).
- An `aggregate` asked for `batchSize: 0` runs nothing at the command: its
  `$out` is written, its errors met and its documents read at the first
  `getMore` (RM-080,
  `tests/backlog/test_rm_found_2026_10_25.py::test_a_first_batch_of_none_runs_nothing_until_the_get_more`).
- A pipeline read through the cursor stage whose last batch fills exactly
  leaves the cursor open, and the next `getMore` is empty, as on mongod
  (RM-080,
  `tests/backlog/test_rm_found_2026_10_25.py::test_a_cursor_stage_that_fills_its_last_batch_exactly_stays_open`).
- A `$group` on `_id` (or any field an ascending index leads with) whose
  accumulators are all `$first` or all `$last` returns its groups in the
  DISTINCT_SCAN's order, read through the cursor stage
  (RM-080,
  `tests/backlog/test_rm_057_084.py::test_aggregate_group_on_id_union_with_and_empty_first_batch_read_on`).
  `explain` takes mongod's rule for it: `$first` beside `$last` is a plain
  `$group`, `$last` scans the other way, a sort on the group's field and
  the accumulated one is served by the compound index, mongod's choice of
  index is the one with the fewest fields that holds every field the group
  reads, and the covered `_id` scan projects `{_id: 1}`. Partly fixes
  RM-309 (`tests/backlog/test_rm_found_2026_10_25.py::test_a_group_read_from_a_distinct_scan_is_planned_and_ordered_as_mongod_does`).

#### Known differences

- RM-309: a DISTINCT_SCAN `$group` down a descending or along a collated
  index, or through a view, still groups in the order documents arrive.
- RM-310: `explain` of a sorted or filtered DISTINCT_SCAN `$group` lists
  no `rejectedPlans`.
- RM-311: `explain` leaves a `$match` on a `$group`'s key behind the group,
  where mongod moves it ahead onto the grouped field.
