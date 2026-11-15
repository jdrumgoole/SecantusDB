### A nested loop over a join lists its clauses as PostgreSQL does

When the upper join of three tables carried more than one clause, EXPLAIN printed its `Join Filter` conjuncts in the order the ON wrote them. PostgreSQL lists them from the outer side's join lists, each base relation's clauses in the order the plan joined them, so `... join e e3 on e2.a < e3.a and e.a < e3.a` now prints `Join Filter: ((e.a < e3.a) AND (e2.a < e3.a))`, as PostgreSQL 15 does.

#### Fixed

- Rust PG server: a nested loop's `Join Filter` orders its ON conjuncts by the first outer relation each names (`build_joinrel_restrictlist`).

Fixes RP-920 (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations with two clauses above])
