### EXPLAIN ANALYZE counts a nested-loop chain over a Hash Join

A nested loop whose outer side is a lower Hash Join now prints that join's
counts as PostgreSQL does: the Hash Join's rows, each scan's, the Hash's
buckets, batches and memory, and the top join's rows removed by its filter.
They printed bare. The lower join's hash table is sized from its inner scan
priced on its own, as the plan above it carries no estimate.

#### Fixed

- EXPLAIN ANALYZE annotates a Nested Loop over a Hash Join (RP-390, part).
  Fixes RP-390, part (tests/backlog/test_rp_explain_costs.py::test_a_chains_lower_hash_join_is_counted)
