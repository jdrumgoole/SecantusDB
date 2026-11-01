### Rust PG server: a descending ORDER BY under a LIMIT over a join keeps PostgreSQL's ties

A `DESC` ORDER BY of one column under a LIMIT over a join was joined whole and
sorted, while PostgreSQL 15.19 plans the join for the LIMIT, as it already did
for an ascending one. Ties came out in another order, so the LIMIT returned
other rows: `t0 JOIN t1 ON t0.b = t1.a ORDER BY t0.b DESC LIMIT 5` over two
300-row tables gave t0's rows 167, 160, 20, 216, 223 where PostgreSQL gives
132, 279, 90, 125, 118.

#### Fixed

- A `DESC` (NULLS FIRST) key goes to the join order search, and every merge
  join on its equivalence class sorts both sides descending, as
  `select_outer_pathkeys_for_merge` takes the query's direction. On the join
  order matrix with every ORDER BY made DESC, against a private 15.19, the
  shapes that differ go from 186 to 71, and none that matched before differs.
  The ascending matrix is unchanged.
- Fixes RP-028, part (tests/backlog/test_rust_postgres_gaps.py::test_a_descending_order_by_under_a_limit_over_two_tables).
  What is left (a DESC key on a LEFT join's nullable side, and a DESC
  ORDER BY with an index in its order, which the search declines) stays
  on RP-028.
