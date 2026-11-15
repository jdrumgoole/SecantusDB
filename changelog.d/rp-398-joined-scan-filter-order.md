### A joined scan's Filter is in PostgreSQL's qual order

A scan under a join now orders its own Filter the way a lone scan already
did: an equality to a constant after the other conjuncts, then by cost.
That holds too for an ON conjunct over a LEFT join's nullable side once the
WHERE reduces the join to an inner one, which now sorts among that scan's
WHERE conjuncts.

#### Fixed

- `select * from s join e on s.a < e.a where e.b = 'v3' and e.a > 5`
  prints `Filter: ((a > 5) AND (b = 'v3'::text))` on `e`'s scan.
  Fixes RP-398 (tests/backlog/test_rp_explain_costs.py::test_plan[a joined scan's filter puts its equality to a constant last], tests/backlog/test_rp_explain_costs.py::test_plan[a reduced left join's scan filter puts its equality to a constant last])
