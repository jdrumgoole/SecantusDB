### Rust PG server: a subquery's join ON can name the outer query

PostgreSQL lets a join's ON inside a subquery read the enclosing query's
columns, as the subquery's WHERE can. So `select * from e where exists
(select 1 from s join k on s.a = k.a and k.b = e.b)` runs on PostgreSQL
15.19. Here it failed with `42P01 missing FROM-clause entry for table "e"`,
and two similar queries failed with `42703 column "b" does not exist`. The
search for outer qualifiers, which is what makes a subquery correlated,
looked at the subquery's select list, WHERE, HAVING, ORDER BY and
FROM-function arguments, but not at its join conditions. A subquery with an
outer reference only in an ON was therefore planned as uncorrelated, and
the reference could not resolve.

#### Fixed

- The search now covers every join's ON, at any depth of the subquery's
  join tree. Such a subquery is evaluated once per outer row, under inner,
  LEFT, RIGHT and FULL joins, and its rows match PostgreSQL 15.19's,
  including for a NULL outer value. A qualifier that names nothing, in the
  subquery or outside it, is still `42P01`.

Fixes RP-430
(tests/backlog/test_rp_column_errors.py::test_subquery_join_on_sees_the_outer_query,
tests/backlog/test_rp_column_errors.py::test_subquery_join_on_outer_reference_rows).

#### Found

RP-437 (a correlated EXISTS over a join whose WHERE reads `k.b = e.b`
matches an outer row whose `e.b` is NULL against an inner NULL, where
PostgreSQL matches nothing), with its gap test.
