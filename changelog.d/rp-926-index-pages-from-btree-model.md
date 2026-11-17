### A nested loop into a deduplicated index is priced at the index's real size

A parameterized index scan repeated once per outer row charges the index
pages all its scans fetch, which for a small index is the whole index. The
Rust PostgreSQL server now sizes that index from its B-tree model, the same
pages `pg_class` reports, so `select * from e join k2 on e.a < k2.y` over an
index of 100 values repeated ten times is PostgreSQL 15.19's `Nested Loop
(cost=0.28..9483.50 ...)`, where it was a page too large at `9487.50`.

#### Fixed

- `dedup_index_size` takes the B-tree model's pages and root level where
  the model holds the index, before its own deduplication estimate.
  Partly fixes RP-926 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop into a weakly correlated index], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of few rows into a weakly correlated index])
