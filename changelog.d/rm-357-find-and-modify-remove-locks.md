### A findAndModify that removes reports mongod's locks

A `findAndModify` with `remove: true` sent without a `txnNumber` now takes
one write round, as mongod 8.2.11 does, so its slow-query line and profile
entry count one IX each on the transition, global, database and collection
locks. This server took a second round inside a first, as an update does.

#### Fixed

- Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_find_and_modify_that_removes_takes_one_round)
