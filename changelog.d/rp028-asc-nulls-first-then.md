### An ascending NULLS FIRST ORDER BY with more keys picks PostgreSQL's join

A joined query under a LIMIT whose ORDER BY starts with an ascending column
NULLS FIRST and has more columns after it now goes through the same join
order search as the other orders. No merge join and no index scan returns
rows in that order, so, as in PostgreSQL, the plan is the cheapest join
under a bounded Sort, and rows that tie on every key come out in the order
that join leaves them. Before, the server planned the join for every row
and sorted it, which could pick a different join and break ties
differently.

#### Fixed

- Rust PG server: `ORDER BY x NULLS FIRST, y ... LIMIT n` over a join
  merges ascending (`Search::merge_asc`) and sorts the cheapest path, as
  15.19 does; the join-order matrix with `--nulls-first --then` matches
  810 of 810 shapes (10 differences before).

Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_nulls_first_key_with_another_sorts_the_whole_join)
