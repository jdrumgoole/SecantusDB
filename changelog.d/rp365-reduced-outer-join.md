### Rust PG server: EXPLAIN prices a join whose WHERE names one table

`EXPLAIN` of a two-table join planned as joined rows, with a WHERE that
names only one of its tables, now prints PostgreSQL 15.19's costs, rows and
widths. Examples are `select * from e join k on e.b = k.b where e.a > 10
and k.a < 900`, and a RIGHT join its WHERE makes inner (`select k.a from e
right join k on e.b = k.b where e.a = 7`). Before this every number was
zero. RP-365 (#2331) fixed this for the narrow two-table path; this covers
the general join planner's joined rows.

#### Fixed

- The joined-rows hash join prices each table's scan through the WHERE
  conditions that name only that table, as PostgreSQL pushes them down
  (`distribute_qual_to_rels`). A condition the planner also keeps above
  the join is counted once.
- The scan's width is its target list: what the join and the rows above
  it read, not the WHERE's columns.
- Not priced: a WHERE over a table with an index, one naming the join key,
  or one that is not plain conditions.
  Follows RP-365 (tests/backlog/test_rp_explain_costs.py::test_plan[a RIGHT join reduced by its WHERE],
  `[a join with a WHERE on each side]`)
