### EXPLAIN ANALYZE counts a nested-loop chain that ends in a Memoize

When a chain of nested loops ended in a Memoize over a parameterized index
scan, EXPLAIN ANALYZE printed the lower join, its scans and the Memoize
bare. They now carry the counts PostgreSQL 15.19 prints: the lower join's
rows and its Join Filter's removed pairs, the Memoize's rows per loop and
its `Hits / Misses / Memory Usage` line, and the index scan's loops, each
taken from the rows the lower join actually produced.

#### Fixed

- EXPLAIN ANALYZE of `z join z z2 on z.a < z2.a join kz on z2.a < kz.a`
  annotates every node below the top Nested Loop.

Fixes RP-390, part (tests/backlog/test_rp_explain_costs.py::test_plan[analyze of a nested loop of three relations into a memoized index])
