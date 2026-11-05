### Rust PG server: a join's DESC NULLS LAST ORDER BY under a LIMIT keeps PostgreSQL's ties

The join search now plans a descending ORDER BY with its NULLs last, as it
already planned an ascending one with them first: an index scan's forward
order is of use for merging only, and a merge join on the ORDER BY's class
sorts both sides descending with NULLs last (`select_outer_pathkeys_for_merge`).
Before, such a query joined every row and sorted, so rows with equal keys
came out in another order than PostgreSQL 15.19's.

#### Fixed

- `t0 JOIN t1 ON t0.b = t1.a ORDER BY t1.a DESC NULLS LAST LIMIT 5` returns
  PostgreSQL's rows among its ties: 1,663 of the join-order matrix's 1,665
  shapes with every ORDER BY made DESC NULLS LAST match PostgreSQL 15.19.
  Part of RP-028; the two left (a LEFT join merging on a backward index
  scan) and an ORDER BY of several columns stay open, with gap tests.

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_descending_nulls_last_order_by_keeps_its_ties)
