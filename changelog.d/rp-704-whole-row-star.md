### Rust PG server: `(v).*` in a select list expands the FROM item's columns

`select (v).* from (values (1, 'x')) v(a, b)` now answers `1, 'x'` under
columns `a` and `b`, as on PostgreSQL 15.19, instead of 0A000 `this field
selection is not supported yet`.

#### Fixed

- A select-list `(v).*` or `(v.*).*`, where `v` names a FROM item of the
  query rather than a column, is that item's star `v.*`, as PostgreSQL's
  `transformIndirection` expands a whole row's star. This works over a
  FROM subquery, a `VALUES` list, a table, a CTE and a multi-column
  function, beside other targets, under DISTINCT, ORDER BY and a set
  operation, and inside a scalar subquery. Before, only an item of a join
  was expanded. A column named `v` still wins, so `(v).*` of an integer
  column stays 42809 `type integer is not composite`, and so does a
  one-column function's alias.
- Fixes RP-704 (tests/backlog/test_rp_found_rp699.py::test_whole_row_star_in_a_select_list);
  crate test `a_whole_row_star_is_the_items_star`.
- Found while working, and filed as RP-852: a star over a FROM subquery in
  a grouped query (`select v.* from (values (1)) v(a) group by a`) is 42803
  with an empty column name.
