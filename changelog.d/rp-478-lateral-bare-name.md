### Rust PG server: a LATERAL subquery reads an earlier FROM item by its bare column name

`select x from fe, lateral (select b as x) s` and `select x from fe join
lateral (select b || c as x from fs) s on true` were 42703 `column "b" does
not exist`: a LATERAL subquery found an earlier FROM item's column only when
it was qualified (`fe.b`). A bare name is now resolved as PostgreSQL's
`colNameToVar` resolves it: against the subquery's own FROM first, at every
level inside it, and against the items to its left only when no nearer level
has the name. A subquery nested inside the LATERAL one reads them too, and a
name two left items share is 42702.

#### Fixed

- A bare column name in a LATERAL subquery, or in any subquery inside one,
  reads the FROM items to its left when no level between has a column of that
  name. A nearer level's own column still wins (`lateral (select b from (select
  'w' as b) d)` is `w`).
- An aggregate over only the left side's columns in a LATERAL subquery whose
  FROM is a function (`lateral (select string_agg(b, ',') from
  generate_series(1, 2))`) is 42803 `aggregate functions are not allowed in
  FROM clause of their own query level`, as it already was over a table: a
  FROM function's columns are now known to that check.

Fixes RP-478 (tests/backlog/test_rp_subquery_scope.py::test_lateral_subquery_reads_an_earlier_item_by_bare_name).

#### Filed

- RP-482: a LATERAL subquery draws a `nextval` once more than its rows, as its
  output's shape is planned by evaluating a FROM-less body.
