### `$all` takes a `$`-prefixed document as a value, as mongod does

An `$all` element that is a document whose first key starts with `$` is not
necessarily an operator. mongod refuses it ("no $ expressions in $all") only
when that key is one of the operators it parses under a field -- `$eq`,
`$gt`, `$in`, `$geoNear`, the `$_internal*` ones and the like -- and takes
any other, such as `{$x: 1}`, `{$not: 1}` or `{$geometry: 1}`, as an equality
to that document. The Rust MongoDB server refused all of them; it now draws
the same line, and a positional update or an upsert built from such a filter
treats the element as the value it is.

#### Fixed

- `{a: {$all: [{$x: 1}]}}` matches a field equal to `{$x: 1}`, or an array
  holding it, on `find`, `aggregate`, `count` and writes, where the Rust
  server refused it with 2. Fixes RM-421
  (tests/backlog/test_rm_found_2026_11_08_rm330.py::test_all_takes_a_dollar_object_as_a_value),
  with tests/mongod_live/test_all_dollar_object.py comparing each command,
  the keywords that are refused and those that are not against mongod 8.2.
- A positional `$set: {"a.$": ...}` whose filter holds such an `$all`
  finds its element, and an upsert from `{a: {$all: [{$x: 1}]}}` seeds
  `a: {$x: 1}`; the upsert seeding lives in `secantus-storage`, which the
  PostgreSQL server links too.

