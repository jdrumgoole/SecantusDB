### Rust MongoDB server: a `$sort` that a later `$sort` overrides is dropped, as on mongod

mongod's pipeline optimizer erases a `$sort` that another `$sort` follows,
so documents tied under the later sort come in the order they had before
the earlier one. This server ran both sorts and its sort is stable, so the
ties kept the earlier sort's order: a different document order from mongod
8.2.11's.

#### Fixed

- A `$sort` directly ahead of another `$sort` is dropped. So is one the
  later sort reaches over stages mongod moves out from between them: a
  `$match` (ahead of an `$unwind` too, when it reads nothing the `$unwind`
  writes), a `$skip` of 0, or an `$unwind` the later sort does not read. A
  `$setWindowFields` that sorts on a field partition or a `sortBy` overrides
  a `$sort` the same way, and a computed partition does not, since mongod
  sets it with an `$addFields` first. A `$limit`, `$addFields`, `$project`
  or any other stage between keeps both sorts.
- The rule applies in `$facet`, `$lookup` and `$unionWith` sub-pipelines,
  and to a view's last `$sort` behind the caller's. `explain` now lists the
  one `$sort` mongod lists mid-pipeline as well.
- Fixes RM-524 (tests/backlog/test_rm_found_2026_11_10_rm410.py::test_a_sort_before_another_sort_is_dropped).
  Regression tests: `test_what_lies_between_two_sorts` and
  `test_a_views_sort_is_overridden` in the same file (both against mongod
  8.2), plus unit tests in `explain::hoist`.
- Fixes RM-517 (tests/backlog/test_rm_found_rm408.py::test_a_sort_a_window_supersedes_is_not_listed):
  `explain` of `[{$unwind: "$t"}, {$sort: {t: 1}}, {$setWindowFields:
  {sortBy: {n: 1}, ...}}]` no longer lists, or counts, the `$sort` on `t`.
- Filed while working on it: RM-633 (a `$match` reading a missing sort key
  says `undefined`, without mongod's executor-error envelope) and RM-634 (a
  `$project` of `{$meta: "sortKey"}` with no `$sort` is refused as
  unsupported rather than with mongod's message).
