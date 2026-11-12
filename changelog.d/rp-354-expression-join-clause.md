### EXPLAIN estimates a join on an expression of an indexed table

A nested loop joined by an inequality over an expression of a table with an index -- `e left join k on e.a < k.a + 1`, where `k` has a primary key -- now prints PostgreSQL's estimate: the clause matches no column index, so it is the join's filter over the table's plain scan, `Nested Loop Left Join (cost=0.00..17532.50 rows=333333 width=14)` as PostgreSQL 15.19 prints it. Before, any inequality that was not between two bare columns left a join over an indexed table at zero cost. Beside a column-to-column bound the expression clause moves into the parameterized index scan as its Filter, as in PostgreSQL. An expression of the outer side against the indexed column (`e.a + 1 < k.a`), which PostgreSQL makes the index's condition, still prints zeros.

#### Fixed

- EXPLAIN of a nested loop over an indexed table joined by an inequality with no bare column of that table, such as `e.a < k.a + 1`, prints PostgreSQL's costs and row estimate instead of zeros (RP-354, part).

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join on an expression of the inner side])
