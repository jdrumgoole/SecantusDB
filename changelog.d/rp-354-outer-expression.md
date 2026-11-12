### EXPLAIN estimates a join that bounds an index by an expression of the outer row

A nested loop whose join clause compares the inner table's indexed column with an expression of the outer row -- `e left join k on e.a + 1 < k.a`, where `k` has a primary key -- now prints PostgreSQL's plan: the inner side is an index scan parameterized by the outer row with `Index Cond: (a > (e.a + 1))`, the expression's operators evaluated once per scan as PostgreSQL's `genericcostestimate` charges them, so PostgreSQL 15.19's `Nested Loop Left Join (cost=0.28..9490.00 rows=333333 width=14)` over `Index Scan using k_pkey on k (cost=0.28..6.14 rows=333 width=7)` is what this server prints too. Before, the join printed zero costs. A clause whose other operand reads the indexed column's own table (`e.a + k.a < k.a`) is no index qual, and now prints as the join's filter over a materialized sequential scan, as PostgreSQL does, instead of zeros.

#### Fixed

- EXPLAIN of a nested loop whose join clause bounds the inner side's indexed column by an expression of the outer row prints PostgreSQL's costs, row estimate and parameterized `Index Cond` instead of zeros (RP-354, part).
- EXPLAIN of a nested loop whose join clause compares a key with an expression of its own table prints PostgreSQL's Join Filter plan instead of zeros (RP-354, part).

Fixes RP-354, in part (tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join into an index on an expression of the outer side], tests/backlog/test_rp_explain_costs.py::test_plan[nested loop left join on a key against an expression of its own table])
