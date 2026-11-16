### A chain of inequality joins runs in the order PostgreSQL picks

An inner join of three or more tables linked only by `<`, `<=`, `>`, `>=`
or `<>` now runs in the join order PostgreSQL's search keeps, not the
order the query wrote. `select * from e join e e3 on e.a < e3.a join zs on
e3.a > zs.a` joins `e3` to the ten-row `zs` first and loops `e` above
them, as PostgreSQL 15.19 does, so its rows come out in the same order,
and EXPLAIN prints that plan with PostgreSQL's costs and each ON as the
Join Filter of the join it moved to. Under a LIMIT the chain still runs
as written.

#### Fixed

- An inner chain of inequality joins (ON or a comma join's WHERE) without
  a LIMIT or ORDER BY runs, and is explained, in the order
  `standard_join_search` keeps (RP-919, in part).

Fixes RP-919 in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations joined in another order], tests/backlog/test_rp_explain_costs.py::test_a_chain_of_inequality_joins_runs_in_the_searched_order)
