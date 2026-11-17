### EXPLAIN VERBOSE prints a filtered hash join's output lists

`EXPLAIN VERBOSE` of a two-table hash join with a WHERE now prints what
PostgreSQL 15.19 prints: each node's `Output` list, each restriction as its
scan's `Filter` with the column prefixed, and under the Hash only the
columns the join reads above it, so a column only the WHERE reads is left
out.

#### Fixed

- Rust PG server: `EXPLAIN VERBOSE` of a hash join whose WHERE conjuncts
  each read one relation prints every node's `Output` and the prefixed
  `Filter`s.
  Fixes RP-966 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a join with a where prints its output under verbose])
