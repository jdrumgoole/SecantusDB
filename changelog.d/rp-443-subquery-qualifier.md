### Rust PG server: a subquery's qualifier that names nothing is PostgreSQL's 42P01, not an outer reference

A qualified column reference inside a subquery is now looked up the way
PostgreSQL's `transformColumnRef` looks it up: in the subquery's own range
table, then in each enclosing query's. A qualifier found at no level is
42P01 `missing FROM-clause entry for table "x"` at the reference, before
anything else is done with the subquery. Until now any qualifier the
subquery's own FROM did not have was treated as a reference to the outer
query, whether or not the outer query had it.

#### Fixed

- `select 1 from k where exists (select x.a from s)` answered rows: an
  EXISTS drops its select list before it runs, and the list was dropped
  before anything read it. The same under NOT EXISTS, a nested EXISTS, an
  EXISTS over one correlated equality (rewritten to a semi-join, which
  also drops the list), and in an UPDATE's or DELETE's subquery. Each is
  42P01 now.
- `select (select x.a from s limit 1)` was 42883 `function
  \x1fcorrelated(...) does not exist`, naming the correlated rewrite's
  internal function. It is 42P01, and so is the schema-qualified
  `public.x.a`.
- `exists (select s.c from s x where x.a = k.a)` answered rows. It is
  42P01 `invalid reference to FROM-clause entry for table "s"` with the
  hint `Perhaps you meant to reference the table alias "x".`
- A correlated reference's error in the outer query is reported at the
  reference, not at the subquery around it: `insert into e values ((select
  x.a from s limit 1), 'x')` is at 31, not 23.

Fixes RP-443 (tests/backlog/test_rp_column_errors.py::test_subquery_qualifier_naming_nothing,
tests/backlog/test_rp_column_errors.py::test_subquery_qualifier_naming_nothing_in_every_clause,
tests/backlog/test_rp_column_errors.py::test_subquery_qualifier_of_an_outer_level_correlates)
