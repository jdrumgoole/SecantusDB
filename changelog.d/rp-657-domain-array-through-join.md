### Rust PG server: a domain column keeps its domain through a join, a WITH and a set operation

A column declared with a domain is of the domain however a query reaches
it, as its Var's `exprType` is on PostgreSQL. Since RP-569 the server cast
such a column to its domain where an array shows its type, but only over a
single-table FROM. It now resolves the reference across the whole FROM
list -- the tables inside a join, a WITH item's columns, a FROM subquery's
columns under its aliases -- and, for an ARRAY subquery over a set
operation, casts every branch when all of them are of the same domain, as
`select_common_type` keeps a type every input shares. A bare name another
FROM item also has, or one a `USING` merges, is left as it was.

#### Fixed

- `array[a]` and `array(select a ...)` over a `char(3)` domain column read
  through a join, a WITH, or a `UNION ALL` of the domain column with
  itself are the domain's array type with the padding kept (`{"a b","xy
  "}`), as on PostgreSQL 15.19, where they were `character[]` and the
  subqueries dropped the blank.
- `pg_typeof(a)` of a domain column in a join or over a WITH names the
  domain, and a cast of such a column to its own domain checks nothing
  (RP-655's relabel now applies there too).

Fixes RP-657 (tests/backlog/test_rp_found_rp569.py::test_domain_column_array_through_join_with_or_set_operation)
