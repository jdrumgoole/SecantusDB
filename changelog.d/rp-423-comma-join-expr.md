### EXPLAIN estimates a comma join over an expression-filtered scan

`explain select e.a from e, w where e.a = w.a and length(e.b) > 2` printed a `Hash Join` of zeros on the Rust PostgreSQL server, where PostgreSQL 15.19 prints `cost=24.16..47.24 rows=333 width=4`. PostgreSQL makes an inner join's WHERE and ON clauses one list, so the comma form plans exactly as the ON form, which the server already estimated. The server re-checks the comma join's equality and the comparison above the join as one residual, and that residual stopped the estimate; it now reads it as the join clause beside the comparisons pushed to each scan. A scan also no longer counts a column in its width that only its pushed comparison reads, as `build_joinrel_tlist` leaves it out.

#### Fixed

- EXPLAIN prices a comma join whose WHERE filters a side by an expression comparison, and the width of a joined scan whose filter reads a column nothing above it uses, as PostgreSQL 15.19 does.

Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a comma join over a scan filtered by a call], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a comma join over a scan filtered by an arithmetic comparison], tests/backlog/test_rp_explain_costs.py::test_plan[an inner join's scan carries no column only its expression filter reads])
