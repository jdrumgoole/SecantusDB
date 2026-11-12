### EXPLAIN estimates a join that bounds an indexed column on both sides

A nested loop whose join clauses bound the inner table's indexed column from both sides -- `e left join k on e.a < k.a and e.a > k.a`, where `k` has a primary key -- now prints PostgreSQL's plan: the inner side is an index scan parameterized by the outer row with both clauses as one `Index Cond: ((a > e.a) AND (a < e.a))`, each clause a third of the rows, so a ninth per scan, as PostgreSQL 15.19 prints `Nested Loop Left Join (cost=0.28..3660.00 rows=111111 width=14)`. Before, two clauses on one index column left the join at zero cost. EXPLAIN ANALYZE counts the inner scan's rows and loops against every bound in the condition.

#### Fixed

- EXPLAIN of a nested loop whose join bounds the inner side's indexed column from both sides prints PostgreSQL's costs, row estimate and two-clause `Index Cond` instead of zeros, and EXPLAIN ANALYZE reports the inner index scan's actual rows and loops (RP-354, part).

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join into an index bounded on both sides])
