### `$lookup` reads `local.oplog.rs` through `from: {db, coll}`

The Rust MongoDB server refused every `$lookup` whose `from` is `{db, coll}`.
mongod 8.2.11 reads two namespaces that way, `local.oplog.rs` and
`config.cache.chunks.*`, and now so does this server; every other one is
still FailedToParse (9), as on mongod.

#### Fixed

- Fixes RM-717 (tests/backlog/test_rm_found_2026_11_17_rm358.py::test_a_lookup_from_the_oplog_runs)
