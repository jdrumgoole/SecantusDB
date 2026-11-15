### Rust MongoDB server: a sort over an `$or` with a fetched branch sorts below the FETCH

An `$or` of a fetched index-scan branch and an index scan that holds the
sort's fields now sorts the OR's members before fetching, as mongod 8.2
does: `SUBPLAN <- FETCH <- SORT (default) <- OR`, where this server sorted
the fetched documents above it.

#### Fixed

- The sort-on-keys check takes a fetched `$or` branch as carrying every
  field (mongod's `hasField`).

Fixes RM-575 (tests/backlog/test_rm_found_rm471.py::test_a_sort_over_an_or_with_a_fetched_branch_sorts_below_the_fetch)
