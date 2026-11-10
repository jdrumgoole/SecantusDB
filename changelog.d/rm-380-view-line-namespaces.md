### Slow-query lines name what mongod's do around a view

A `Slow query` line names a namespace and the kind of collection behind it,
and around a view this server named them as it found them rather than as
mongod's operation acquired them. A `getMore` of a view's cursor, a write to
a view or a time-series collection, and a `drop` or `collMod` of a view now
say `collectionType: "normal"`, as on mongod; only a read of a view or a
time-series collection that succeeds says `view` or `timeseries`.

A read of a view that fails once it runs is now filed under the collection
beneath the view, on its line and in its profile entry, because mongod runs
the read there. An `explain` of a `find`, `aggregate`, `count` or
`distinct` now names the collection it explains rather than `<db>.$cmd`.

#### Fixed

- `secantus-commands` (`slowop`): a line's `collectionType` is `view` or
  `timeseries` only for a read that succeeded (`find`, `aggregate`,
  `count`, `distinct`, or an `explain` of one) and `normal` otherwise;
  a write statement's line is `normal` too (`profiler::record`).
- `secantus-commands` (`slowop`, `profiler::record`): a read of a view that
  failed after resolving the view names the collection at the end of its
  dependency chain (`<db>.c`, or `<db>.system.buckets.ts` for a time-series
  collection) on its line and in its profile entry (`opdebug::beneath_view`).
- `secantus-commands` (`slowop`): an `explain` line names the collection of
  the read it explains; an `explain` of a write still names `<db>.$cmd`.
- `secantus-commands` (`profiler::record`): a profile entry for a `getMore`
  of a view's or a time-series collection's cursor counts its batch
  (`nBatches: 1`).
- `secantus-commands` (`profiler::record`): a view's `distinct`, which
  answers an int32 `ok`, is no longer profiled as a failure (`ok: 0` and an
  empty `errMsg`); the profiler reads `ok` with `util::reply_ok`.
- Fixes RM-380 (tests/backlog/test_rm_view_lines.py::test_a_get_more_of_a_view_is_of_a_normal_collection,
  tests/backlog/test_rm_view_lines.py::test_a_find_and_modify_of_a_view_is_of_a_normal_collection,
  tests/backlog/test_rm_view_lines.py::test_an_explain_line_names_what_it_explains,
  tests/backlog/test_rm_view_lines.py::test_a_read_of_a_view_that_fails_names_its_collection);
  regression comparisons in tests/mongod_live/test_view_line_namespaces.py.
- Filed while working it: RM-483 to RM-491 (tests/backlog/test_rm_view_namespaces.py).
