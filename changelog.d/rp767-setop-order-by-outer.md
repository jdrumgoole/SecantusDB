### A set operation's ORDER BY refuses an outer column as PostgreSQL does

The ORDER BY of a UNION, INTERSECT or EXCEPT inside a correlated subquery
sees only the set operation's output columns. A name that resolves to the
outer row is a sort expression there, which PostgreSQL refuses as 0A000
`invalid UNION/INTERSECT/EXCEPT ORDER BY clause`; the Rust PostgreSQL server
now gives the same error, detail and position instead of 42703.

#### Fixed

- Rust PG server: `order by b` / `order by v.b` naming an outer column in a
  correlated set operation is 0A000 with PostgreSQL's DETAIL, at the column.
  Fixes RP-767 (tests/backlog/test_rp_found_rp698.py::test_an_outer_column_in_a_set_operations_order_by)
