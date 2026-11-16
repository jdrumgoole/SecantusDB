### A sorted chain of inequality joins runs in PostgreSQL's order

A chain of three or more tables joined by inequalities and sorted by an
ORDER BY now runs in the join order PostgreSQL's search keeps, and EXPLAIN
prices it as PostgreSQL does: the chain's cheapest plan under a Sort whose
cost is bounded by the LIMIT. The Sort Key of any join's ORDER BY now
prints each column qualified by its relation, as PostgreSQL's does.

#### Fixed

- Rust PG server: an ORDER BY over a chain of inequality joins keeps
  PostgreSQL's join order and prices its Sort and Limit; a join's Sort Key
  prints qualified columns.

Fixes RP-919, in part (tests/backlog/test_rp_explain_costs.py::test_plan[a chain of inequality joins under an order by])
