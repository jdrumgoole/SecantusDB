### Rust MongoDB server: an `$or` of index scans answers a covered projection from its keys

`find({$or: [{y: 0}, {y: 1, x: {$gt: 3}}]}, {_id: 0, x: 1})` over `{y: 1, x: 1}` is now planned as mongod 8.2.11 plans it: `SUBPLAN <- PROJECTION_DEFAULT <- OR <- IXSCAN...`, with no FETCH and `totalDocsExamined: 0`. Before, the server fetched every document (`PROJECTION_SIMPLE <- FETCH <- OR`). The planner now follows mongod's `fetched` / `hasField` rules for the scan it reads. An `OR` or `SORT_MERGE` is fetched only when every branch is. A branch that fetched carries every field, and an index scan carries the fields its index holds. A projection whose fields every branch carries, and whose sort (if any) those branches hold, runs over the `OR` without a FETCH. This also holds when some branches fetched (`OR <- [FETCH <- IXSCAN, IXSCAN]`), under a `SORT_MERGE`, a blocking sort, a limit, a skip and `showRecordId`. It is mongod's general `PROJECTION_DEFAULT`, since only a single index scan's keys make a `PROJECTION_COVERED`. The same rule means an `OR` whose every branch fetched no longer takes a second FETCH above it (`SUBPLAN <- OR <- [FETCH <- IXSCAN...]`), in `explain` and in a profile entry's `execStats`.

#### Fixed

- Fixes RM-471 (tests/backlog/test_rm_found_2026_11_09_rm373.py::test_an_or_of_scans_answers_a_covered_projection)
- Fixes RM-459 (tests/backlog/test_rm_found_2026_11_09_rm349.py::test_a_rooted_or_of_tried_branches_has_no_fetch_over_its_or)
- Regression cases in `crates/secantus-storage-adapter/tests/explain_commands_wt.rs::sort_paging_and_projection_wrap_the_scan`.

#### Filed

- RM-569, RM-570 and RM-572 to RM-576, found while probing this against mongod 8.2.11 (`tests/backlog/test_rm_found_rm471.py`). They cover: covered rows' field order, a computed projection over keys, an `$or` over two indexes returning its branches out of order, an `{_id: 1}` projection not covered, a clause beside an `$or` rechecked in a FETCH, a sort over an `$or` with a fetched branch, and a rooted `$or`'s profile `planSummary`.
- RM-571 (`returnKey` kept its projection, over one scan and over an `$or`) was found the same way and was already fixed by RM-470's change on the integration branch. Its test, `tests/backlog/test_rm_found_rm471.py::test_return_key_drops_the_projection`, stays as a regression test for those two shapes.
