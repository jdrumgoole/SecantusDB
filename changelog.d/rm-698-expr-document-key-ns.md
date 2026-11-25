### An `$expr` on `documentKey`, `ns` or `to` after a change stream joins the oplog read

The Rust MongoDB server now rewrites a `$match` of an `$expr` on the
event's `documentKey`, `ns` or `to` into the change stream's oplog filter,
as mongod 8.2.11 does, so `explain` prints the same read and `queryHash`.

#### Fixed

- An `$expr` over `$documentKey`, `$ns` or `$to` (and their sub-paths)
  becomes an `$expr` over the oplog entry, and the `$_internalExpr*`
  predicates the optimizer derives from it become predicates on `o`,
  `o2`, `ns` and `o.to`, on `fullDocument` too.

Fixes RM-698, in part (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_an_expr_on_the_document_key_or_namespace_joins_the_oplog_filter)
