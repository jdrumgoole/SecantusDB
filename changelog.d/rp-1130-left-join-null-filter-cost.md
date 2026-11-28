### Rust PG server: a LEFT join filtered on its nullable side `IS NULL` is priced

A WHERE that tests a nullable-side column other than the join key `IS
NULL` leaves the join outer, because a null test is not strict. PostgreSQL
runs that test as the join's own Filter: it costs nothing
(`cost_qual_eval` charges a `NullTest` nothing), and its selectivity, the
column's null fraction, scales the join's rows after the outer join's
clamp. The server drew that plan but printed zero for every cost, row
count and width. It now prices it as PostgreSQL does, a restriction of the
preserved side included. The rows returned do not change.

#### Fixed

- `explain select * from e left join e e2 on e.a = e2.a where e2.b is
  null` prints PostgreSQL 15's `Hash Left Join (cost=27.50..56.25 rows=1
  width=14)`, and over a column a quarter NULL, `rows=250`.
- Over an expression key on the nullable side (`on e.a = e2.a + 1`) it is
  the `Hash Right Join (cost=27.50..57.50 rows=1 width=14)` hashing the
  preserved side, where an unpriced Nested Loop Left Join printed before.
- Beside a restriction of the preserved side (`and e.b = 'v1'`) it is
  PostgreSQL's `Hash Right Join (cost=18.75..38.50 rows=1 width=14)`.

Fixes RP-1130 (tests/backlog/test_rp_explain_costs.py::test_plan[a left join filtered on its nullable side's other column is priced], tests/backlog/test_rp_explain_costs.py::test_plan[a left join on an expression key filtered on its nullable side's other column is priced])
