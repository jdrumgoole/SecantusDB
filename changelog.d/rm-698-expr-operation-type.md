### A change stream's `$expr` on `operationType` joins the oplog read

The Rust MongoDB server now rewrites an `$expr` on `$operationType` in the
`$match` after a `$changeStream` into the oplog filter, as mongod 8.2.11
does, so the stream's `explain` and plan-cache `queryHash` match mongod's.

#### Fixed

- An `$expr` comparing `$operationType` (`$eq`, `$ne`, `$gt`, `$in`, ...,
  under `$and` / `$or` / `$not`) is rewritten into an `$expr` over the oplog
  entry, with `$operationType` as mongod's `$switch` on `op`, and the
  `$_internalExprEq` mongod's optimizer derives from an `$eq` joins the read
  as the `operationType` equality's rewrite. An `$expr` on other event
  fields is still not rewritten (RM-698 stays open for them).

Fixes RM-698, part (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_an_expr_on_the_operation_type_joins_the_oplog_filter)
