### Rust PG server: a scalar subquery can be a member of an `IN` list

`x in ((select max(y) from t), 'tt')` is an ordinary list to PostgreSQL,
each member compared by `=`. The Rust PostgreSQL server refused it with
0A000 `SubLink is not supported yet`, because the pass that resolves
scalar subqueries before planning did not look inside an `IN` list. It does now, so an uncorrelated member becomes one more
value of the list and a correlated one is evaluated per row, in WHERE and
in the select list alike.

#### Fixed

- A scalar subquery as a member of an `IN` / `NOT IN` list answers as
  PostgreSQL 15.19 does, including a NULL or empty subquery, more than one
  row (21000) and a member whose type has no `=` with the left side
  (42883).

Fixes RP-757 (tests/backlog/test_rp_found_rp707.py::test_a_scalar_subquery_in_an_in_list)
