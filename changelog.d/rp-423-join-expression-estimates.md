### EXPLAIN estimates a join whose WHERE filters a side by an expression

`explain select * from e join w on e.a = w.a where e.a + 1 > 996` printed zeros for every node on the Rust PostgreSQL server. A single table's scan already priced a comparison of an expression with a constant as its per-row residual, but a join's side refused any residual, and the join refused the copy of the conjunct it re-checks above itself. The planner now collects each joined table's expression comparisons by the columns' qualifiers, and the side's scan is priced with them as a lone scan's is. The scans, the Hash and the join print PostgreSQL 15.19's costs and rows, with the smaller side hashed.

#### Fixed

- EXPLAIN of an inner join whose WHERE compares an expression over one table's columns with a constant prints PostgreSQL 15.19's estimates.

Fixes RP-423 in part (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a join over a scan filtered by an arithmetic comparison], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a join over two scans each filtered by an arithmetic comparison], tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a join over a scan filtered by a range and an arithmetic comparison])
