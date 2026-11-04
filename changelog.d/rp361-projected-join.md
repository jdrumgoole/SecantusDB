### Rust PG server: EXPLAIN draws a join that selects some columns

`EXPLAIN` of a two-table join whose select list names columns, such as
`select e.a from e join k on e.b = k.b`, now prints PostgreSQL 15.19's plan:
a `Hash Join` with its `Hash Cond`, the right table's scan under a `Hash`,
and each `Seq Scan` named by its table and alias. When both tables are
analysed, it also prints PostgreSQL's cost, rows and width. Before this the
plan was a `Nested Loop` over two `Seq Scan` lines naming no relation, all
zeros.

#### Fixed

- The narrow two-table join path draws its real tree: a `Hash Join` (or
  `Hash Left Join`) for an ON equality and a `Nested Loop` for a cross
  join. Its inner join on one column equality is priced as RP-330 and
  RP-359 price a joined-rows hash join. A WHERE, a LEFT join and a cross
  join still print zeros (RP-362).
  Fixes RP-361 (tests/backlog/test_rp_explain_costs.py::test_plan[hash join reading one side's column])
