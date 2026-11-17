### EXPLAIN prices a WHERE join filter over both tables of an ON join

PostgreSQL makes an inner join's ON and WHERE one list of quals, so a WHERE
inequality over both tables, `select e.a from e join w on e.a = w.a where
e.a + w.a > 996`, is the hash join's Join Filter exactly as the same
conjunct in the ON would be. EXPLAIN now prints that join with PostgreSQL
15.19's costs and rows, `Hash Join (cost=27.50..62.25 rows=333 width=4)`,
where it printed zeros.

#### Fixed

- EXPLAIN of an inner ON join whose WHERE holds an inequality over both
  tables prices it as the join's filter. RP-423 stays open for a regex whose
  prefix walk is not modelled and a call whose overloads differ in cost.
  Trims RP-423 (tests/backlog/test_rp_explain_costs.py::test_plan[estimate of a join filtered by an expression over both tables])
