### Rust MongoDB server: an `$or` over one index sorts its keys below the FETCH

A rooted `$or` whose branches all read one index, such as
`find({$or: [{y: 0}, {y: {$gt: 1}}]}).sort({x: 1})` over `{y: 1, x: 1}`,
is one index scan of the branches' intervals. mongod 8.2.11 sorts its
index keys below the FETCH (`SUBPLAN <- FETCH <- SORT default <- IXSCAN`);
this server sorted the fetched documents above it.

#### Fixed

- The planner asks the merged scan's own index whether it holds the sort.
  Fixes RM-557 (tests/backlog/test_rm_found_rm476.py::test_an_or_of_one_index_sorts_its_keys)
