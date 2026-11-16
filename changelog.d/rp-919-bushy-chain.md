### A chain of inequality joins runs as the bushy tree PostgreSQL keeps

A chain of four or more tables joined by inequalities, under a LIMIT, now
runs as the tree PostgreSQL's join search keeps even when that tree is
bushy: two joins built separately, one the Materialize inner of the
other. EXPLAIN prints and prices that tree as PostgreSQL 15.19 does. Every
nested loop of such a chain now also reads the side the plan names as its
outer first, so rows past the first few come out in PostgreSQL's order
where a two-table join's written left had been its outer.

#### Fixed

- A bushy chain of inequality joins under a LIMIT ran in its written
  order and printed unpriced; a chain's two-table join ran its written
  left as the outer when the plan's outer was the right.
  Fixes RP-919, part (tests/backlog/test_rp_explain_costs.py::test_plan[a bushy chain of inequality joins under a limit], tests/backlog/test_rp_explain_costs.py::test_a_bushy_chain_runs_each_loop_outer_first)
