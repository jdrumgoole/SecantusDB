### Rust PG server: a correlated set operation reads the outer row

A scalar, EXISTS, IN or ARRAY subquery that is a UNION, INTERSECT or
EXCEPT can now read the outer query's columns from any of its arms, as on
PostgreSQL: `select (select v.a intersect select 5) from (values (5), (6))
v(a)` answers `5` and NULL instead of failing.

#### Fixed

- A set-operation subquery whose arms name an outer column (`v.a`) was
  42P01 `missing FROM-clause entry for table "v"`; it is now correlated
  through either arm and evaluated per row.
- A scalar set-operation subquery is named for its leftmost arm's first
  column (`a`), where it was `?column?`.
- A bare name in a set-operation arm whose own FROM has that column is the
  arm's column: `(select a from (values (1)) t(a) union select 9 order by 1
  limit 1)` over an outer `a` answered the outer row's value, a wrong
  answer. Only an arm with no FROM reads a bare name from the outer row.

Fixes RP-698 (tests/backlog/test_rp_found_rp517.py::test_correlated_set_operation)
