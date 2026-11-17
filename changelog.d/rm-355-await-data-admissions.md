### Rust MongoDB server: an `awaitData` getMore on a capped collection waits instead of spinning

A tailable `awaitData` cursor on a capped collection reported oplog
position 0, so its wait for data returned at once and the getMore rescanned
the collection in a loop until `maxTimeMS`, taking an execution ticket each
time: 24,845 admissions on its `Slow query` line for a 500 ms wait, where
mongod 8.2.11 counts 4. The tail now records the oplog position it scanned
under and sleeps until a later write, in one ticketless wait preceded by one
yield, as mongod yields twice per wait.

#### Fixed

- Fixes RM-355 in part (tests/backlog/test_rm_355_364.py::test_an_await_data_get_more_takes_mongods_execution_tickets,
  tests/backlog/test_rm_355_364.py::test_an_await_data_get_more_woken_twice_takes_mongods_tickets)
