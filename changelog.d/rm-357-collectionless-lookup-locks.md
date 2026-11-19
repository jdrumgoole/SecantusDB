### A collectionless aggregate's `$lookup` and `$unionWith` count mongod's locks

An `aggregate: 1` holds no lock while its pipeline runs, so on mongod
8.2.11 each read a `$lookup` or `$unionWith` makes of its collection takes
the global IS twice of its own. The Rust MongoDB server's slow-query line
now counts them as mongod does.

#### Fixed

- A collectionless `aggregate` whose `$lookup` or `$unionWith` reads a
  collection reports mongod's `Global` acquisitions on its line: once, then
  twice per read. A `$lookup` reads once for all the outer documents unless
  its first stage is correlated (the equality form, or a `let` variable
  there), as mongod's sub-pipeline cache does. It counted one.

Fixes RM-357 in part (tests/backlog/test_rm_355_364.py::test_a_collectionless_aggregate_reads_under_no_lock)
