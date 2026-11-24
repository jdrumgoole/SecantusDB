### A view's DDL is logged as mongod logs it

`secantusd-mdb` now logs creating, changing and dropping a view as mongod
8.2.11 does: as writes to the database's `system.views` (an insert of the
definition, an update replacing it, its delete, with the first view's
`create: "system.views"` ahead of them). There is no longer a command naming
the view. mongod can now replay a killed store whose workload made views;
before, a view's `create` entry carried the internal `viewPipeline` option,
which mongod refused, and that aborted its recovery.

#### Fixed

- The view change events are derived from those writes, as mongod derives
  them. Under `showExpandedEvents` a database or cluster stream sees
  `create` (`nsType: "view"`), `modify` and `drop`, each with
  `operationDescription: {viewOn, pipeline}` and no `collectionUUID`. A
  stream without expanded events sees none of them, and neither does a
  stream on the view itself. Before, each event carried the view's
  internal options, and `modify` had an empty description.
- Crash recovery replays the `system.views` writes as the views' DDL.

Fixes RM-745 (tests/backlog/test_rm_storage_model.py::test_a_views_ddl_is_logged_as_mongod_logs_it,
tests/backlog/test_rm_storage_model.py::test_mongod_opens_a_killed_store_whose_workload_made_views,
tests/backlog/test_rm_storage_model.py::test_a_killed_store_replays_its_views)
