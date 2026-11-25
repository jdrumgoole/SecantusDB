### An `IndexKeySpecsConflict` quotes the requested spec in mongod's order

The spec an index conflict quotes lists `unique`, `hidden` and
`prepareUnique` before `key`, then `name`, then the other options, as `mongod`
8.2.11 writes it whatever order the client sent them in.

#### Fixed

- The requested spec of an `IndexKeySpecsConflict` (86) was quoted with every
  option before `key`. It is `{ v: 2, unique: true, key: ..., name: ... }` and
  `{ v: 2, key: ..., name: ..., sparse: true }`. Closed by the change that
  keeps `sparse: false` (RM-788), which wrote the message's spec in creation
  order.

Fixes RM-772 (tests/backlog/test_rm_found_rm732.py::test_an_index_conflict_quotes_the_requested_spec_in_mongods_order)
