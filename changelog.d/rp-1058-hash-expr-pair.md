### Rust PG server: EXPLAIN prices a hash join on an expression key beside other clauses

A join on an expression key and a column pair (`e.a = k.a + 1 and e.b =
k.b`), or on two expression keys, is now priced as PostgreSQL prices it:
each equality one hash clause, the expression's side with no statistics
(`eqjoinsel` on the default 200 distinct values), and each clause's
operators charged in the clause cost. Before, only a join on one expression
key and nothing else was priced; with a second clause every cost printed as
zero and the expression's side was hashed rather than the side PostgreSQL
picks.

#### Fixed

- `explain select e.b from e join k on e.a = k.a + 1 and e.b = k.b` prints
  PostgreSQL 15.19's plan: `Hash Join (cost=30.00..54.75 rows=100
  width=3)`, `e` hashed, `Hash Cond: (((k.a + 1) = e.a) AND (k.b = e.b))`.
  The clauses in the other order, an expression of the left table, and two
  expression keys (`... and e.b = k.b || 'x'`, a Hash Join rather than an
  unpriced Nested Loop) match too.
- A join whose expression key shares a column with another clause (`e.a =
  k.a - 1 and e.a = k.a`) stays unpriced: PostgreSQL makes it one
  equivalence class and a filter on `k`, filed as RP-1117.

Fixes RP-1058 (tests/backlog/test_rp_explain_costs.py::test_plan[a hash join on an expression and a column pair hashes the column side])
