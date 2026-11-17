### A NOT EXISTS is planned as a Hash Anti Join in EXPLAIN

`explain select * from e where not exists (select 1 from e e2 where e2.a
= e.a)` now prints PostgreSQL's `Hash Anti Join` on `(e.a = e2.a)`, its
inner side hashed at the width of the one column it reads, priced as
PostgreSQL prices it (`rows=1`). As `convert_EXISTS_sublink_to_join`
does, the sublink is pulled up into an anti join, here through the LEFT
join form the server already plans as one. A NOT EXISTS with other
conjuncts beside it is still planned as a filter.

#### Fixed

- EXPLAIN of a WHERE that is one `NOT EXISTS` over a column equality
  prints a priced Hash Anti Join.
  Fixes RP-980 (tests/backlog/test_rp_explain_costs.py::test_plan[a not exists over an equality is a hash anti join])
