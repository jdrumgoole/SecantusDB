### EXPLAIN estimates nested loops of four or more tables, and `<>` joins of three

EXPLAIN used to price a nested-loop join of exactly three tables, and only when every clause was a `<`, `<=`, `>` or `>=`. It now prices a left-deep join of three to eight unindexed tables, each clause linking two of them. The search over the tables is PostgreSQL's `standard_join_search`, level by level. Each level joins every relation of the level below with each table a clause links it to. From four tables up, bushy pairs of two smaller joins are weighed too. Every way of building a relation competes in that relation's one path list, and its row count is set by the first pairing built. A `<>` clause is now one of these clauses at any level, its selectivity `neqjoinsel` over the two columns' statistics. PostgreSQL 15.19 prints `select * from e join e e2 on e.a < e2.a join e e3 on e2.a < e3.a join e e4 on e3.a < e4.a` (1,000 analysed rows each) as three priced Nested Loops topped by `cost=0.00..1671680062.50 rows=37037000000 width=28`, and this server now prints the same plan. With `e2.a <> e3.a` as the upper clause of three tables, both print `rows=332999667`.

#### Fixed

- EXPLAIN of an inner join of three to eight unindexed tables by `<`, `<=`, `>`, `>=` or `<>` clauses, in the order PostgreSQL keeps, prints PostgreSQL's costs, rows and widths (RP-354, part).

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations with a <> above], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of four relations], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop of three relations by <> alone])
