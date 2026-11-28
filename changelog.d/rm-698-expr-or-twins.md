### A change stream's `$expr` `$or` joins the oplog read as mongod's does

An `$expr` `$or` after a `$changeStream`, such as one over `txnNumber`, now
derives the `$_internalExpr*` twins mongod 8.2.11's `RewriteExpr` does: an
`$or` of each branch's twins, none when a branch derives none.

#### Fixed

- Fixes RM-698 (tests/backlog/test_rm_found_2026_11_15_rm452.py::test_an_expr_or_on_a_renamed_field_joins_the_oplog_filter_as_mongod_does)
