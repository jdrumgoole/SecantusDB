### A join whose WHERE pins one column to two constants plans no scan

On the Rust PostgreSQL server, EXPLAIN of an inner join whose WHERE equates
one column to two different constants now prints what PostgreSQL 15.19 does:
a bare `Result` with `One-Time Filter: false`. The column's equivalence
class is contradictory, so its relation is empty, and so is the join.

#### Fixed

- EXPLAIN of an inner (or comma) join whose WHERE equates a column to two
  distinct constants is a `Result` with `One-Time Filter: false`, not the
  join's scans. The same in a LEFT join's ON is still open (RP-400).

Fixes RP-400 in part (tests/backlog/test_rp_explain_costs.py::test_plan[a join whose where equates a column to two constants], tests/backlog/test_rp_explain_costs.py::test_plan[a comma join of aliases whose where equates a column to two constants]).
