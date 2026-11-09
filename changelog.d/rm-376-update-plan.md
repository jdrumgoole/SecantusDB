### An update is planned as mongod plans it, under a sort or a collation

A write by `_id` skips the query planner on mongod only when it runs under
the `_id` index's own collation. Under any other, mongod plans it like any
query: a numeric `_id` is read through the `_id` index (`IXSCAN { _id: 1 }`),
and a string `_id` by a collection scan. The Rust MongoDB server took the
express path whatever the collation was. An update with a `sort` (new in
8.0) is now read in the sort's order, as mongod reads it: through an index
that gives the order, or a SORT over every match. Before this change the
server picked the document first and then reported an `_id` express write.
Profile entries, slow-query lines and `explain` now show mongod's plan and
its keys and documents examined.

#### Fixed

- A write by `_id` under a collation other than the `_id` index's no longer
  takes the express or IDHACK path. This covers updates, upserts, deletes
  and findAndModify. The planner's `_id` index now carries the collection's
  default collation, as `listIndexes` already showed. Before, an update on
  such a collection with `collation: {locale: "simple"}` was reported as
  express.
- An update with a `sort` is planned and counted with the sort (UPDATE over
  SORT, or over LIMIT and an index read in order). `explain` of such an
  update now includes the sort, and the update's index-use count is taken
  with it.
- Fixes RM-376 (tests/backlog/test_rm_found_2026_11_07.py::test_an_update_is_planned_as_mongod_plans_it,
  tests/backlog/test_rm_found_rm376.py::test_a_write_takes_the_id_fast_path_only_under_the_id_indexs_collation,
  tests/backlog/test_rm_found_rm376.py::test_an_update_reads_under_its_sort).
- Filed RM-474: a write's LIMIT of one reports a second work.
