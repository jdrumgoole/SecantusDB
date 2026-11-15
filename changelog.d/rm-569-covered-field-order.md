### Rust MongoDB server: a covered row's fields come in the index key's order

A `find` whose plan answers from an index key now returns each row's fields
in the key pattern's order, as mongod builds the row of the key.

#### Fixed

- `{_id: 0, x: 1, y: 1}` read covered from `{y: 1, x: 1}` returned
  `{x, y}`, the document's order; mongod 8.2.11 returns `{y, x}`. Under an
  `$or`, a row from an unfetched branch now takes its key's order, and one
  from a fetched branch keeps the document's, as on mongod.
  Fixes RM-569 (tests/backlog/test_rm_found_rm471.py::test_covered_fields_come_back_in_the_key_patterns_order),
  regression test tests/mongod_live/test_covered_field_order.py.
