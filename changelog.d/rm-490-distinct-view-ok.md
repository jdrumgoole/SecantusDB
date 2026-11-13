### A `distinct` of a view is no longer counted as a failure

A `distinct` of a view answers `ok` as an int32, as mongod 8.2's does, where
every other reply's `ok` is a double. The Rust MongoDB server read a reply's
`ok` as a double in 22 places, so it took that reply for a failure:
`serverStatus` counted it in `metrics.commands.distinct.failed`, where mongod
does not, and the transaction, interrupt, write-concern, slow-query,
`$queryStats` and session paths read it the same way. Each now reads `ok` as a
number whatever its BSON type, as mongod does.

#### Fixed

- `secantus-commands`: a new `util::ReplyOk::ok_number` reads a reply's `ok`
  as a number, whatever its BSON type, and every reader of a reply's `ok`
  outside the tests uses it; `secantus-server`'s `top` reset after a `drop`
  uses its own `reply_ok`.
- Fixes RM-490 (tests/backlog/test_rm_view_namespaces.py::test_a_distinct_of_a_view_is_not_counted_failed)
