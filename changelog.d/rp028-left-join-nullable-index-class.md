### A LEFT join ordered by its preserved key keeps the forward index scan

An outer join's clause makes a one-member equivalence class for each of its
two keys, and the Rust PostgreSQL server's join search gave both the same id.
Under a DESC NULLS LAST ORDER BY of the preserved side's key with another key
after it, the nullable side's index scan was therefore taken to be in the
ORDER BY's order and replaced by its backward scan, and the plan merged on
it, leaving ties in another order than PostgreSQL's Merge Right Join over
the forward scan. The nullable side's scan is no longer in that order.

#### Fixed

- `t0 LEFT JOIN t1 ON t0.b = t1.a ORDER BY t0.b DESC NULLS LAST, t1.c
  LIMIT 5` with an index on `t1.a` returns PostgreSQL's rows.

Fixes RP-028, in part (tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_preserved_desc_nulls_last_key_with_another)
