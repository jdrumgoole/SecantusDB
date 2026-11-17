### A join on an expression key is a priced Hash Join

`explain select * from e join e e2 on e.a = e2.a + 1` now prints
PostgreSQL's `Hash Join (cost=27.50..57.50 rows=1000 width=14)` with
`Hash Cond: ((e2.a + 1) = e.a)`, hashing the side whose key has
statistics and probing with the expression, where it used to print an
unpriced Nested Loop. The expression has no statistics, so it is
estimated as PostgreSQL does: 200 distinct values, no common values, its
operators charged on every hash-clause comparison. A LEFT join on one
plans as a `Hash Right Join`, and one whose WHERE keeps only the rows
nothing matched as a `Hash Anti Join (cost=27.50..133.88 rows=500)`.

#### Fixed

- EXPLAIN prices a join of two tables on one equality between a column
  and an operator expression over the other table as PostgreSQL's hash
  join, inner, LEFT or anti; a `Hash Cond` with an expression operand is
  printed outer side first.
  Fixes RP-981 (tests/backlog/test_rp_explain_costs.py::test_plan[an inner join on an expression key is a hash join], tests/backlog/test_rp_explain_costs.py::test_plan[a left join on an expression key where the nullable side is null is a hash anti join])
