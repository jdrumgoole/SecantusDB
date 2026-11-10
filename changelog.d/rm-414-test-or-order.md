### RM-414's regression test ignores mongod's `$or` branch order

`tests/backlog/test_rm_found_2026_11_08b.py::test_a_collectionless_explain_reorders_and_counts_as_mongod` compares the explain of a collectionless `$documents` pipeline with mongod's. mongod 8.2.11 lists the branches of the `$or` it moves ahead of `$documents`' stages in an order that changes from call to call: 15 of 30 explains gave one order and 15 the other, on one server and across restarts. Snapshots #2652 and #2655 failed on it. Both servers' `$or` branches are now compared in one canonical order.

#### Fixed

- `test_a_collectionless_explain_reorders_and_counts_as_mongod` no longer fails on mongod's run-to-run `$or` order.
