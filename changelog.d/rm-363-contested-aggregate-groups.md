### A contested aggregate that groups counts what it examined

The Rust MongoDB server's slow-query line and profile entry for an
`aggregate` whose `$match` two indexes contest, followed by a `$group`, now
count the keys and documents the plan mongod's trial chose examined, as
mongod 8.2.11's do (133 of each over 400 documents), where they counted
nothing.

#### Fixed

- A grouping pipeline front, which mongod runs in SBE, now runs the trial's
  winner after its buffered results instead of reading through the store's
  router. Fixes RM-363 in part
  (tests/backlog/test_rm_355_364.py::test_a_contested_aggregate_that_does_not_stream_counts_what_it_examined,
  tests/backlog/test_rm_355_364.py::test_a_contested_group_run_again_counts_what_its_cached_plan_examined).
