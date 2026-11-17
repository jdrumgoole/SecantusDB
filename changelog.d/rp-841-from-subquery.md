### An INTERSECT over a FROM subquery hashes the smaller input first

An INTERSECT returns, for each group, the row its first input gave, and
PostgreSQL puts the input with fewer estimated groups first. A plain FROM
subquery over a table is pulled up into the scan it reads, so it is
estimated like that scan: `select x from (select x from t) s intersect
select 1.000` now returns `1.000`, as PostgreSQL 15.19 does, where it
returned the subquery's `1.0`.

#### Fixed

- A FROM subquery that is a plain select over one user table is estimated
  as its pulled-up scan when an INTERSECT orders its inputs.
  Fixes RP-841 in part (tests/backlog/test_rp_found_rp692.py::test_an_intersect_over_a_from_subquery_puts_the_smaller_input_first)
