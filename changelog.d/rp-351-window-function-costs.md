### Rust PG server: EXPLAIN prices aggregates and argument-taking window functions

`EXPLAIN` of a WindowAgg now prints PostgreSQL 15.19's `cost_windowagg`
estimate for any built-in window function or aggregate whose argument is a
plain column -- `sum(a) over ()`, `lag(a) over (order by a)`, `count(b)`,
`string_agg(b, ',')` -- where it printed zeros for everything except
`row_number`, `rank` and `dense_rank`.

#### Fixed

- The WindowAgg's width is its target list's, as in PostgreSQL: each
  function adds its result type's width (`lag(int4)` is 4, not 8), and a
  column that only a function's argument reads is not counted.
- Fixes RP-351 (tests/backlog/test_rp_explain_costs.py::test_plan[an aggregate over a window], tests/backlog/test_rp_explain_costs.py::test_plan[a window function with an argument])

#### Found

- RP-352: a FILTER clause, an expression argument and stacked windows are
  still printed with zeros.
