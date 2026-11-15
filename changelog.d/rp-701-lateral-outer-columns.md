### A LATERAL subquery names and expands its left items' columns

A LATERAL subquery's select list is now read as PostgreSQL's
`transformTargetList` reads it before the references to the left become
per-row parameters: a column of a left FROM item is named by its column, as
`FigureColname` names it, rather than by its type, and a star over a left
item expands to that item's columns instead of being refused as an invalid
FROM-clause reference.

#### Fixed

- Rust PG server: `select l.* from (values (3)) v(a), lateral (select v.a)
  l` names its column `a` (it was `int4`), `lateral (select fe.a, fe.b) l`
  names `a`, `b` (`int4`, `text`), and a set operation is named by its
  first arm; `lateral (select v.*) l` and `lateral (select fe.* from fs) l`
  answer the left item's columns (each was 42P01), a side of a USING join
  with its copy of the merged column.
- Fixes RP-701 (tests/backlog/test_rp_found_rp517.py::test_lateral_outer_columns)
