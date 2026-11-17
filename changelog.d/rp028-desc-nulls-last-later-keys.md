### A DESC NULLS LAST ORDER BY of several columns over a join merges on a backward index scan

A join under a LIMIT whose ORDER BY starts with a descending key, its NULLs
last, and continues with more keys now takes PostgreSQL's plan when that
first key is an inner join's: the merge join reads the key's index
backward, in its descending order, and the bounded sort on top settles the
later keys. Before, the join was planned for every row and sorted, which
left rows tying on the first key in another order, so a LIMIT returned
different rows from PostgreSQL's.

#### Fixed

- Rust PG server: an ORDER BY `k DESC NULLS LAST, ...` over a join, where
  `k` is an inner join's key with an index, weighs the merge join on the
  index's backward scan. Fixes RP-028, part
  (tests/backlog/test_rust_postgres_gaps.py::test_a_desc_nulls_last_key_with_another_sorts_the_merge).
