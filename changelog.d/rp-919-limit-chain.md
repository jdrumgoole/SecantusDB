### A chain of inequality joins under a LIMIT runs in PostgreSQL's order

An inner join of three or more tables linked only by inequalities now
returns its rows in the order PostgreSQL's plan does when the query has a
LIMIT. The join search keeps, for each join relation, the paths that are
cheaper to start beside the one cheapest in total, as PostgreSQL does when
a LIMIT makes startup cost matter, and the top path is the one the LIMIT
reads least of. Before, a LIMIT stopped the search and the tables joined in
their written order, so `LIMIT 5` returned different rows.

#### Fixed

- Rust PG server: an inequality join chain under a LIMIT runs in the order
  PostgreSQL's search keeps (`consider_startup` path lists,
  `get_cheapest_fractional_path`). Fixes RP-919 in part
  (tests/backlog/test_rp_explain_costs.py::test_a_chain_of_inequality_joins_under_a_limit).
