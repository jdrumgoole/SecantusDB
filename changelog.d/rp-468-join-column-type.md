### Rust PG server: a correlated subquery reading a join's column keeps its type

A scalar subquery can read a column of the query around it. When that column
belonged to a JOIN in the enclosing FROM, this server returned it as text.
`select (select fs.a from fe where fe.a = fs.a) from fs join fe using (a)`
answered `'1'` where PostgreSQL answers the integer `1`. The same happened
through a join's alias (`j.a`) and a USING alias (`u.a`). The lookup of an
outer reference's type covered tables, FROM-subqueries, CTEs and a write's
target, but not a join.

#### Fixed

- A correlated subquery's outer reference into a JOIN is typed the way
  PostgreSQL's join range table entry types it: a leaf's own column, a USING
  or NATURAL join's merged column, which takes the two sides' common type
  (`bigint` over `int4` and `int8`), or a column of the join's alias. A leaf
  that names a CTE is that CTE, ahead of a table of the same name. Measured on
  PostgreSQL 15.19.

Fixes RP-468 (tests/backlog/test_rp_subquery_scope.py::test_join_column_in_a_subquery_keeps_its_type)
