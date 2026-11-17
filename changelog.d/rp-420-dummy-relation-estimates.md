### A dummy join keeps its width, and a count over a dummy relation is priced

When a WHERE folds to constant false, PostgreSQL still sizes and prices
what sits around the empty `Result` it plans. A join made dummy keeps the
width of the columns it carries (`select * from e, k where e.a = null` is
`Result (cost=0.00..0.00 rows=0 width=14)`), and `select count(*) from e
where a = null` is `Aggregate (cost=0.00..0.01 rows=1 width=8)`, as
PostgreSQL 15.19 prints them.

#### Fixed

- The Rust PostgreSQL server's dummy `Result` carries a join's width (a
  single table's stays 0, as PostgreSQL excludes it before sizing it), and
  a plain count above it is priced by `cost_agg` over no rows. A Sort above
  it and an UPDATE's dummy Result stay open under RP-420.
  Partly fixes RP-420 (tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over a dummy relation is priced], tests/backlog/test_rp_explain_costs.py::test_plan[a dummy join keeps its width])
