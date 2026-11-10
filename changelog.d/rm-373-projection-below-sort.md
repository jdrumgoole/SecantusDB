### A find's projection runs below its sort, as mongod's does

When a find both projects and sorts in memory, mongod 8.2 usually projects
first and sorts the projected documents. The rule: the sort has no limit,
the projection only includes or excludes fields, and the projection keeps
every path the sort orders by. The Rust MongoDB server used to do this only
for an aggregate's `$project`. It now follows the same rule for every read,
in `explain` and in a profile entry's `execStats`, and its SORT counts the
smaller projected documents in `totalDataSizeSorted`, as mongod's does.

#### Fixed

- **Placement.** A find's projection now goes below a blocking SORT
  (`SORT <- PROJECTION_SIMPLE <- COLLSCAN`; covered,
  `SORT <- PROJECTION_COVERED <- IXSCAN`) when all of these hold:
  - the sort has no limit;
  - every leaf of the projection is an include or exclude flag (dotted,
    sub-document and positional paths still count);
  - the projection keeps every sort path whole.

  A literal, an expression, `$slice`, `$elemMatch`, `$meta` or
  `showRecordId` keeps the projection above the sort. So does a limit, or
  a sort path the projection drops or only partly keeps.
- **Sizing.** A SORT over a projection sizes the projected documents, both
  in `explain` and when the read runs. It now gives mongod's numbers: 318
  bytes where the whole documents come to 492, for example.
- **Names.**
  - A projection with a literal or string value (`{z: 1, q: null}`,
    `{_id: "x"}`, `{w: "$x"}`) is `PROJECTION_DEFAULT`, not
    `PROJECTION_SIMPLE`.
  - A covered projection over a SORT_MERGE is `PROJECTION_DEFAULT`, not
    `PROJECTION_COVERED`.
  - Beside `showRecordId`, a read's projection is `PROJECTION_DEFAULT`, and
    its `transformBy` carries `$recordId: {$meta: "recordId"}`.
- Fixes RM-445
  (tests/backlog/test_rm_found_2026_11_09.py::test_a_renaming_projection_is_projection_default):
  `[{$project: {w: "$x"}}]` is `PROJECTION_DEFAULT`, by the naming rule
  above.
- Fixes RM-373
  (tests/backlog/test_rm_found_2026_11_07.py::test_a_projection_is_where_mongods_plan_puts_it).
  The gap test's `simple` read had sorted on a field its index returns in
  order, so mongod planned no SORT for it. It now sorts on `z`.
- Regression tests:
  - `tests/mongod_live/test_projection_below_sort.py` compares 34 finds and
    5 aggregates with mongod 8.2.11, plus the `totalDataSizeSorted` of
    seven reads in both `explain` and the profiler.
- Filed while working:
  - RM-470: `returnKey` placement above a sort.
  - RM-471: an `$or` of scans answering a covered projection.
  - RM-472: a sort beside `showRecordId` is `default`.
  - RM-473: a SORT over a covered projection counts its working-set
    members.
