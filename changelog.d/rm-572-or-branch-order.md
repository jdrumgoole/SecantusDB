### Rust MongoDB server: an `$or` over two indexes returns its branches in mongod's order

`find({$or: [{y: 0}, {x: {$gt: 8}}]})` over `{y: 1, x: 1}` and `{x: 1, y: 1}`
now returns the `y_1_x_1` branch first, as mongod 8.2.11 and this server's
own explain do, so a sort with a limit over it keeps the same rows.

#### Fixed

- The planner's index catalog came from the store sorted by name, while
  mongod's (and `listIndexes`) is in build order; the planner sorts an
  `$or`'s branches by catalog position, so a read ran them in a different
  order from the plan explain printed. The store's planner read now uses
  `listIndexes`' order. The PostgreSQL server links the changed storage
  crate but does not read this list.
  Fixes RM-572 (tests/backlog/test_rm_found_rm471.py::test_an_or_of_two_indexes_returns_its_branches_in_order)
