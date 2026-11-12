### Rust MongoDB server: a wrong-type error's expected types are checked as a set

RM-465 asked the server to list a numeric field's expected types in mongod
8.2.11's order. Probing every numeric slot showed that order is not one
mongod keeps, so the item closes as a scope decision with no change to what
the server answers.

#### Fixed

- The type SET already matched mongod 8.2.11 on every numeric slot probed
  (`find`, `getMore`, `count`, `create`, `distinct`, `insert`, `explain`,
  `aggregate` and its `cursor.batchSize`). Only the order differed, and
  mongod's order differs per IDL file within one build and moves between
  patch builds (8.2.1, 8.2.11 and 8.2.12 all differ), so no fixed order is
  mongod's. The gap test now compares the list as a set, as
  `tests/mongod_reference.py` already does, and covers `count.skip` and
  `create.size` as well.
- Fixes RM-465 (tests/backlog/test_rm_aggregate_parse.py::test_wrong_type_lists_mongods_set_of_types).
