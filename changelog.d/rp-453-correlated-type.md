### Rust PG server: a correlated subquery over a CTE or a write's target keeps its type

A correlated subquery that read the outer row from a CTE, or from an
INSERT's, UPDATE's or DELETE's target in its RETURNING, sent its value as
text: `with c as (select 1 as a) select (select c.a from fs limit 1) from c`
answered `'1'` where PostgreSQL answers the integer 1. The outer
reference's type was looked up only among the enclosing FROM's tables and
FROM-subqueries. It is now found in the enclosing statements' CTEs as well
(a CTE hiding a table of its name, as in PostgreSQL), and in a write's
range table.

#### Fixed

- A correlated subquery's value read from a CTE, or from a write's target
  in its RETURNING, has the column's type, not `text`.
- `array(select c.a from ...)` over a CTE's column answered `{NULL}`; it
  answers the value.
- A CTE named like a table (`with fs as (select 'q'::text as a)`) typed a
  subquery's `fs.a` by the TABLE's column, so the text was sent as an
  integer the client could not decode.

Fixes RP-453 (tests/backlog/test_rp_subquery_scope.py::test_correlated_value_keeps_its_type)
