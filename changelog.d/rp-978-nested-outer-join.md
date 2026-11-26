### EXPLAIN prices an inner join nested in a LEFT join, in PostgreSQL's order and method

`secantusd-pg` drew `t LEFT JOIN (x JOIN y ON ..) ON ..` as written, every
node `cost=0.00..0.00 rows=0 width=0`, often with a method and order
PostgreSQL would not pick. EXPLAIN now plans it as PostgreSQL 15 does. The
inner join is priced as the two-table join it is, then the LEFT join is
chosen from the methods PostgreSQL weighs over the inner join's cheapest
path. That can be a nested loop over it materialised or not, a hash join
hashing either side, or a merge join, each priced as for two tables.

#### Fixed

- `explain select * from s left join (e join k on e.a < k.a) on s.a = e.a
  and k.b = 'v3'` drew a `Hash Left Join` hashing the inner join. It now
  prints PostgreSQL's `Hash Right Join (cost=1.68..1669.42 rows=1000
  width=34)`, the inner join's `Nested Loop` over a `Materialize` of `k`
  as its outer side and `s` hashed.
- `... left join (e join k on e.a > 998 and k.a = e.a) on true` drew a Hash
  Join of two Seq Scans under the loop. It now prints the `Nested Loop Left
  Join (cost=0.28..36.16 rows=60 width=34)` over a `Materialize` of the
  nested loop into `k_pkey`. `EXPLAIN ANALYZE` counts every node below the
  top: the Materialize's `actual rows=2 loops=30`, the inner join run once
  under it.
- A plan whose order already matched (`s left join (e join s s2 on e.a =
  s2.a) on s.a = e.a and s2.c = 'xx'`) now carries its estimates. The
  inner join's width counts the LEFT join's key, but the LEFT join's width
  counts only the columns the query reads, as PostgreSQL's target lists
  do.
- Fixes RP-978 in part (`tests/backlog/test_rp_explain_costs.py::test_plan[analyze of an inner join under an outer join with a one-table ON conjunct]`,
  `test_plan[a left join's ON conjunct over a table of an inner join it null-extends]`
  and four more). Still open under RP-978: a LEFT join keyed on an indexed
  column of the inner join, where PostgreSQL also weighs a path
  parameterized by the preserved table, and ANALYZE's counts below a hash
  join over the inner join. Files RP-1121: VERBOSE of such a plan prints no
  `Output` lines.
