### A collectionless aggregate takes no lock

An `aggregate: 1` whose pipeline reads no collection — `$documents`,
`$currentOp`, `$listLocalSessions` — runs on mongod without taking the
global lock, so its `Slow query` line and profile entry print `locks: {}`,
its line counts no execution admission and has no `storage` section, and the
line names the namespace `<db>.$cmd.aggregate`. The Rust MongoDB server now
reports these operations the same way.

#### Fixed

- A collectionless `aggregate` whose pipeline has no `$lookup`,
  `$unionWith`, `$graphLookup`, `$out`, `$merge` or `$changeStream` (at any
  depth) took the global IS, an execution ticket with it, and a `storage`
  section on its line. It now takes no lock, as on mongod 8.2.11.
- The `Slow query` line of a collectionless `aggregate` named `<db>.$cmd`.
  It now names `<db>.$cmd.aggregate`, as mongod's line and this server's
  profile entry already did.
- RM-357 stays open for the remaining writes whose locks differ, and for a
  collectionless aggregate whose `$lookup` or `$unionWith` reads a
  collection.

Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_collectionless_aggregate_takes_no_lock)
