### A set operation over a join with a WHERE puts the smaller input first

An INTERSECT or EXCEPT whose input is an inner join of two tables with a
WHERE comparing one side's column with a constant is now estimated as
PostgreSQL estimates it: each side's rows are its filtered scan's, the
join's rows those times the join clause's selectivity. The input with fewer
estimated groups runs first, so `select a.x from t692j a join t692j b on
a.k = b.k where a.x > 0 intersect select 1.000` returns the constant's
`1.000`, as PostgreSQL 15.19 does, instead of the join's `1.0`.

#### Fixed

- Rust PG server: a set operation's input that joins two tables never
  analysed under a WHERE over one side's column is estimated
  (`join_input_estimate`), so the smaller input is the first.
  Trims RP-841 (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_join_with_a_where_puts_the_smaller_input_first)
