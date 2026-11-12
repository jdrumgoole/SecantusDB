### Rust MongoDB server: `returnKey` is the top of the plan, and keeps its filter

A `find` with `returnKey` now plans its RETURN_KEY where mongod 8.2.11
does: above the blocking sort and the skip, below a limit no sort
absorbed, with the projection ignored. Before, a read no index served
ignored `returnKey` in its plan, and a read whose index did not hold the
sort put RETURN_KEY below the sort with no FETCH, which dropped the
filter the FETCH applies.

#### Fixed

- `find({y: {$gte: 0}, z: 1}).sort({z: 1}).returnKey()` over
  `{y: 1, x: 1}` returned the keys of all twelve documents, the six with
  `z: 0` included; it now filters `z` in a FETCH below the sort and
  returns the six, as mongod does (`RETURN_KEY <- SORT <- FETCH <-
  IXSCAN`).
- `explain` of a `returnKey` read over a COLLSCAN shows `RETURN_KEY <-
  SORT <- COLLSCAN` (or `RETURN_KEY <- COLLSCAN`) where it showed the
  projection or nothing, and a `showRecordId` beside `returnKey` no
  longer adds a projection.

Fixes RM-470 (tests/backlog/test_rm_found_2026_11_09_rm373.py::test_return_key_is_the_top_of_a_sorted_plan,
tests/backlog/test_rm_found_rm470.py::test_return_key_keeps_the_residual_filter,
crates/secantusdb/tests/streaming_cursors.rs::return_key_over_a_document_sort_keeps_the_filter)

#### Found

- RM-566: a SORT under `returnKey` is `simple` where mongod's is
  `default`, and its top-k leaves ties in another order
  (tests/backlog/test_rm_found_rm470.py::test_a_sort_under_return_key_is_a_default_sort).
- RM-567: a `returnKey` point lookup on `_id` plans an IXSCAN where
  mongod plans IDHACK
  (tests/backlog/test_rm_found_rm470.py::test_return_key_of_an_id_point_lookup_is_idhack).
- RM-568: `returnKey` drops a projection's `{$meta: "sortKey"}` field
  (tests/backlog/test_rm_found_rm470.py::test_return_key_keeps_a_sort_key_meta_field).
