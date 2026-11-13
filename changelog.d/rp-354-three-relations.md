### EXPLAIN estimates a nested loop of three tables

A join of three tables written `a JOIN b ON .. JOIN c ON ..`, where every clause is a `<`, `<=`, `>` or `>=` between two of them, now prints PostgreSQL's two priced Nested Loops instead of zeros. The estimate follows PostgreSQL's own search over the three tables. Each pair of tables that a clause links becomes a two-table join, built in FROM order and priced by its cheapest nested loop. Each of those is then joined with the third table, and every pairing's nested loops compete in the one relation's path list. The three-table join's row count comes from the first pairing built, and each join's width counts only the columns read above it. PostgreSQL 15.19 prints `select * from e join e e2 on e.a < e2.a join e e3 on e2.a < e3.a` (1,000 analysed rows each) as `Nested Loop (cost=0.00..5015045.00 rows=111111000 width=21)` over the `e`-`e2` Nested Loop and a Materialize of `e3`. A join PostgreSQL would run in another order, an indexed table, a `<>` clause or a fourth table still prints zeros.

#### Fixed

- EXPLAIN of an inner join of three unindexed tables by inequality clauses, in the order PostgreSQL keeps, prints PostgreSQL's costs, rows and widths, with each inner Materialized where that is cheaper (RP-354, part).

#### Added

- Backlog item RP-919: a join of three tables by inequalities runs in its written order (its rows come out in a different order from PostgreSQL's), and a comma join's WHERE clauses all run at the top join. Filed with gap tests.
- Backlog item RP-920: the upper join of three tables prints its clauses in the ON's order, not PostgreSQL's. Filed with a gap test.

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations reading one column], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations with the lower join reordered], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations with a where on one])
