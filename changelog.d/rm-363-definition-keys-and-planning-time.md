### A view's definition counts its key, and every plan is timed

mongod keeps each view, and each time-series collection, as a document in
its database's `system.views`. Creating one inserts that document's `_id`
index key, and dropping it deletes the key. The Rust MongoDB server already
presented `system.views` that way, but its `Slow query` lines and profile
entries counted no such key. They now say `keysInserted: 1` for the
`create`, `keysDeleted: 1` for the `drop`, and `keysInserted: 0,
keysDeleted: 0` for a `collMod` of a view, which mongod writes in place. A
plain collection's `create` and `drop` still count no key, as on mongod.

A `count` with no query, and a read of a collection that does not exist,
now carry `planningTimeMicros` like any other read. mongod plans both: a
`RECORD_STORE_FAST_COUNT` for the first and an `EOF` for the second. This
server settled them without its planner and printed no planning time. It
now times the step where it settles them.

#### Fixed

- `secantus-storage`: creating, dropping and running `collMod` on a
  collection whose options define it in `system.views` (`viewOn` or
  `timeseries`) adds the `_id` key of that definition to the running
  operation's `keysInserted` and `keysDeleted`. The PostgreSQL server links
  the same storage crate; nothing it prints reads these counts.
- `secantus-commands`: the `count` fast path and the missing-collection
  `find` record their planning time (`explain::planned_apart`).
- Fixes part of RM-363
  (tests/backlog/test_rm_355_364.py::test_creating_a_view_inserts_its_definition,
  tests/backlog/test_rm_355_364.py::test_a_definition_in_system_views_counts_its_key,
  tests/backlog/test_rm_355_364.py::test_a_plan_settled_without_the_planner_is_timed,
  crates/secantus-storage/tests/system_views_keys.rs). Still open: the key
  and document counts of a read whose plan mongod picks by trial (it waits
  on RM-349) and of a time-series read (it waits on RM-348).
- `tests/mongod_live/test_index_builds.py`: a build is taken as started
  once its `startIndexBuild` is in the oplog and its operation is listed.
  mongod lists the operation while it is still setting the build up, and a
  `killOp` that landed then interrupted the setup, with no oplog entry,
  where the test asks for a started build's abort (seen once in the gate).
- Filed RM-383: a `dropDatabase` line names `<db>.$cmd` where mongod names
  `<db>`.
- Filed RM-384: dropping a view writes no profile entry.
