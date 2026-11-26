### A change stream's `$expr` on `clusterTime`, `lsid` or `txnNumber` joins the oplog read

The Rust MongoDB server now rewrites an `$expr` after a `$changeStream` on
`clusterTime` (as `$ts`), `lsid` or `txnNumber` into the oplog read's events
and its transaction filter, beside the `$_internalExpr*` twins mongod 8.2.11
derives from it, so the read's `queryHash` and filter match mongod's.

#### Fixed

- An `$expr` on a renamed change-stream field was left out of the oplog read.
  Part of RM-698 (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_an_expr_on_a_renamed_field_joins_the_oplog_filter).
