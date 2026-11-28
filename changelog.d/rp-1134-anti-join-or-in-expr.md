### Rust PG server: an anti join beside an outer OR, IN list or expression comparison is priced in EXPLAIN

`EXPLAIN` of a `NOT EXISTS` beside an OR (`where (a > 900 or a < 10) and not
exists (...)`), an IN list (`a in (1, 2, 3)`) or an expression comparison
(`length(b) > 1`) on the outer table, and of their LEFT JOIN spellings, now
prints PostgreSQL's plan: a priced `Hash Anti Join` whose outer `Seq Scan`
carries the condition as its `Filter`, with the rows it keeps. The
expression comparison printed an unpriced `Hash Left Join` with `Filter:
(e2.a IS NULL)` before. An inner join whose WHERE restricts its key with an
IN list is priced too.

#### Fixed

- A restriction of a hash join side that names the join key is priced on
  that side's scan where it is an OR or an IN list of two or more items,
  as a range already was: only an equality with a constant joins the
  key's equivalence class and replaces the join clause. `a IN (1)`, which
  PostgreSQL makes `a = 1`, still does.
- A LEFT JOIN whose WHERE tests the nullable side's join key `IS NULL` is
  recognised as an anti join beside expression comparisons over the
  preserved side, which its scan runs and prices.

Fixes RP-1134 (tests/backlog/test_rp_explain_costs.py::test_plan[a not exists beside an outer or is a hash anti join], tests/backlog/test_rp_explain_costs.py::test_plan[a not exists beside an outer in list is a hash anti join], tests/backlog/test_rp_explain_costs.py::test_plan[a not exists beside an outer expression comparison is a hash anti join]; regressions tests/backlog/test_rp_explain_costs.py::test_plan[an in list over an inner join's key restricts its scan], tests/backlog/test_rp_explain_costs.py::test_plan[a left join anti form beside an outer expression comparison]). Files RP-1242 (an OR restricting an inner or LEFT hash join's side is unpriced) and RP-1243 (a strict expression over a LEFT join's nullable side does not make it inner).
