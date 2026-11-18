### A descending ORDER BY of a LEFT join's nullable key, with more keys, is planned as PostgreSQL plans it

A LIMIT query ordered first by a descending key that a LEFT join NULL-extends, or that no join clause reads, followed by further keys, used to skip the join-order search and fall back to joining every row and sorting. PostgreSQL merges on such a key in ascending order, or reads an index backward, and those plans leave ties in an order a client can see under the LIMIT. The search now plans these queries too, so they return the same rows as PostgreSQL 15.

#### Fixed

- Rust PG server: `ORDER BY <nullable key> DESC [NULLS LAST], <other keys> LIMIT n` over a LEFT join is searched under the ascending-merge model instead of declined.
  Fixes part of RP-028 (tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_desc_nulls_last_key_with_another_sorts_the_merge)
